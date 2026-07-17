use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    DebugPoint, DebugPointKind, Diagnostic, DiagnosticPhase, DiagnosticSeverity, RevisionId,
    RuntimeHost, SnapshotLimitError, SnapshotLimits, SnapshotReason, SourceDocument, SourceId,
    SourceSpan, VmSnapshot, snapshot, vm,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    Continue,
    StepInto,
    StepOver,
    StepOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionState {
    Ready,
    Running,
    Paused,
    Completed,
    Faulted,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    DebugPoint,
    Step,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActivationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PauseLocation {
    pub source_id: SourceId,
    pub revision: RevisionId,
    pub span: SourceSpan,
    pub debug_point_id: crate::DebugPointId,
    pub activation_id: ActivationId,
    pub dynamic_event: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOperation {
    StartDebugging,
    RunAll,
    Resume(ResumeMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionError {
    pub operation: SessionOperation,
    pub state: ExecutionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Paused(PauseReason),
    Completed,
    Faulted(Diagnostic),
    Cancelled,
    Rejected(SessionError),
}

#[derive(Debug, Default)]
struct ControlState {
    pause: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionControl {
    inner: Arc<ControlState>,
}

impl ExecutionControl {
    pub fn request_pause(&self) {
        self.inner.pause.store(true, Ordering::Release);
    }

    pub fn request_cancel(&self) {
        self.inner.cancel.store(true, Ordering::Release);
    }

    pub fn cancel(&self) {
        self.request_cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.inner.cancel.load(Ordering::Acquire)
    }

    fn acknowledge_pause(&self) -> bool {
        self.inner.pause.swap(false, Ordering::AcqRel)
    }
}

#[derive(Debug, Clone)]
struct StepPlan {
    mode: ResumeMode,
    start_activation: ActivationId,
    ancestors: Vec<ActivationId>,
    minimum_epoch: u64,
}

pub struct InterpreterSession<H: RuntimeHost> {
    document: SourceDocument,
    host: H,
    vm: vm::VM,
    state: ExecutionState,
    control: ExecutionControl,
    pause_location: Option<PauseLocation>,
    last_arrival: Option<(ActivationId, crate::DebugPointId, usize, u64)>,
    dynamic_event: u64,
    opcode_epoch: u64,
    step_plan: Option<StepPlan>,
    initial_debug_pause: bool,
    snapshot_limits: SnapshotLimits,
    latest_snapshot: Option<VmSnapshot>,
}

impl<H: RuntimeHost> InterpreterSession<H> {
    pub fn new(document: SourceDocument, host: H) -> Self {
        Self::with_snapshot_limits(document, host, SnapshotLimits::default())
            .expect("default snapshot limits are valid")
    }

    pub fn with_snapshot_limits(
        document: SourceDocument,
        host: H,
        snapshot_limits: SnapshotLimits,
    ) -> Result<Self, SnapshotLimitError> {
        snapshot_limits.validate()?;
        Ok(Self {
            document,
            host,
            vm: vm::VM::new(),
            state: ExecutionState::Ready,
            control: ExecutionControl::default(),
            pause_location: None,
            last_arrival: None,
            dynamic_event: 0,
            opcode_epoch: 0,
            step_plan: None,
            initial_debug_pause: false,
            snapshot_limits,
            latest_snapshot: None,
        })
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn control(&self) -> ExecutionControl {
        self.control.clone()
    }

    pub fn execution_state(&self) -> ExecutionState {
        self.state
    }

    pub fn pause_location(&self) -> Option<&PauseLocation> {
        self.pause_location.as_ref()
    }

    pub fn snapshot(&self) -> Option<&VmSnapshot> {
        self.latest_snapshot.as_ref()
    }

    pub fn start_debugging(&mut self) -> RunOutcome {
        self.start(SessionOperation::StartDebugging, true)
    }

    pub fn run_all(&mut self) -> RunOutcome {
        self.start(SessionOperation::RunAll, false)
    }

    pub fn resume(&mut self, mode: ResumeMode) -> RunOutcome {
        if self.state != ExecutionState::Paused {
            return self.rejected(SessionOperation::Resume(mode));
        }

        let location = self
            .pause_location
            .take()
            .expect("paused state has a location");
        let chain = self.vm.activation_chain();
        let start_index = chain
            .iter()
            .position(|id| *id == location.activation_id)
            .expect("paused activation remains installed");
        self.step_plan = match mode {
            ResumeMode::Continue => None,
            ResumeMode::StepInto | ResumeMode::StepOver | ResumeMode::StepOut => Some(StepPlan {
                mode,
                start_activation: location.activation_id,
                ancestors: chain[..start_index].to_vec(),
                minimum_epoch: self.opcode_epoch.saturating_add(1),
            }),
        };
        self.state = ExecutionState::Running;
        self.drive()
    }

    fn start(&mut self, operation: SessionOperation, debugging: bool) -> RunOutcome {
        if self.state != ExecutionState::Ready {
            return self.rejected(operation);
        }
        self.state = ExecutionState::Running;

        let mut compilation = CompilationHost::default();
        match self.vm.prepare(&self.document, &mut compilation) {
            Ok(()) => {}
            Err(vm::StartError::Compile) => {
                let diagnostic =
                    compilation
                        .diagnostics
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Diagnostic {
                            phase: DiagnosticPhase::Compiler,
                            severity: DiagnosticSeverity::Error,
                            code: "compiler.error".to_string(),
                            message: "Compilation failed without a diagnostic.".to_string(),
                            span: self.document.eof_span(),
                            frames: Vec::new(),
                        });
                let request = snapshot::SnapshotRequest {
                    reason: SnapshotReason::Faulted,
                    active_offset: None,
                    current_span: diagnostic.span,
                };
                let snapshot = match self.vm.build_snapshot(request, &self.snapshot_limits) {
                    Ok(snapshot) => snapshot,
                    Err(_) => return self.finish_snapshot_failure(diagnostic.span),
                };
                self.latest_snapshot = Some(snapshot);
                if compilation.diagnostics.is_empty() {
                    self.host.diagnostic(diagnostic.clone());
                } else {
                    for value in compilation.diagnostics {
                        self.host.diagnostic(value);
                    }
                }
                self.vm.cleanup_execution();
                self.state = ExecutionState::Faulted;
                return RunOutcome::Faulted(diagnostic);
            }
            Err(vm::StartError::Runtime(fault)) => return self.finish_fault(fault),
        }

        self.initial_debug_pause = debugging;
        self.drive()
    }

    fn drive(&mut self) -> RunOutcome {
        loop {
            if self.control.is_cancelled() {
                return self.finish_cancelled();
            }

            let semantic_point = match self.vm.current_semantic_point() {
                Ok(point) => point,
                Err(_) => return self.finish_snapshot_failure(self.document.eof_span()),
            };
            if let Some((activation_id, point)) = semantic_point {
                let arrival = (activation_id, point.id, point.offset, self.opcode_epoch);
                if self.last_arrival != Some(arrival) {
                    self.last_arrival = Some(arrival);
                    self.dynamic_event = match self.dynamic_event.checked_add(1) {
                        Some(event) => event,
                        None => {
                            return self.finish_fault(vm::VmFault::new(
                                "Debug event counter exhausted.",
                                point.offset,
                            ));
                        }
                    };

                    let initial_debug_pause = std::mem::take(&mut self.initial_debug_pause);
                    let reason = if self.control.acknowledge_pause() {
                        Some(PauseReason::Explicit)
                    } else if initial_debug_pause {
                        Some(PauseReason::DebugPoint)
                    } else if self.step_is_complete(activation_id) {
                        Some(PauseReason::Step)
                    } else {
                        None
                    };

                    if let Some(reason) = reason {
                        return self.pause(reason, activation_id, point);
                    }
                }
            }

            if self.control.is_cancelled() {
                return self.finish_cancelled();
            }

            match self.vm.dispatch_one(&mut self.host) {
                Ok(vm::DispatchResult::Continue) => {
                    self.opcode_epoch = match self.opcode_epoch.checked_add(1) {
                        Some(epoch) => epoch,
                        None => {
                            return self.finish_fault(vm::VmFault::new(
                                "Instruction counter exhausted.",
                                self.vm.current_offset().unwrap_or(0),
                            ));
                        }
                    };
                }
                Ok(vm::DispatchResult::Complete) => {
                    self.state = ExecutionState::Completed;
                    self.pause_location = None;
                    self.step_plan = None;
                    self.latest_snapshot = None;
                    return RunOutcome::Completed;
                }
                Err(fault) => return self.finish_fault(fault),
            }
        }
    }

    fn step_is_complete(&self, current_activation: ActivationId) -> bool {
        let Some(plan) = &self.step_plan else {
            return false;
        };
        if self.opcode_epoch < plan.minimum_epoch {
            return false;
        }

        match plan.mode {
            ResumeMode::Continue => false,
            ResumeMode::StepInto => true,
            ResumeMode::StepOver => {
                if self.vm.contains_activation(plan.start_activation) {
                    current_activation == plan.start_activation
                } else {
                    plan.ancestors.contains(&current_activation)
                }
            }
            ResumeMode::StepOut => {
                !self.vm.contains_activation(plan.start_activation)
                    && plan.ancestors.contains(&current_activation)
            }
        }
    }

    fn pause(
        &mut self,
        reason: PauseReason,
        activation_id: ActivationId,
        point: DebugPoint,
    ) -> RunOutcome {
        let request = snapshot::SnapshotRequest {
            reason: SnapshotReason::Paused(reason),
            active_offset: Some(point.offset),
            current_span: point.span,
        };
        let snapshot = match self.vm.build_snapshot(request, &self.snapshot_limits) {
            Ok(snapshot) => snapshot,
            Err(_) => return self.finish_snapshot_failure(point.span),
        };
        self.latest_snapshot = Some(snapshot);
        self.state = ExecutionState::Paused;
        self.step_plan = None;
        self.pause_location = Some(PauseLocation {
            source_id: point.span.source_id,
            revision: point.span.revision,
            span: point.span,
            debug_point_id: point.id,
            activation_id,
            dynamic_event: self.dynamic_event,
        });
        RunOutcome::Paused(reason)
    }

    fn finish_fault(&mut self, fault: vm::VmFault) -> RunOutcome {
        let fault_offset = fault.offset;
        let diagnostic = match self
            .vm
            .try_diagnostic_for_fault(fault, self.document.eof_span())
        {
            Ok(diagnostic) => diagnostic,
            Err(_) => return self.finish_snapshot_failure(self.document.eof_span()),
        };
        let request = snapshot::SnapshotRequest {
            reason: SnapshotReason::Faulted,
            active_offset: Some(fault_offset),
            current_span: diagnostic.span,
        };
        let snapshot = match self.vm.build_snapshot(request, &self.snapshot_limits) {
            Ok(snapshot) => snapshot,
            Err(_) => return self.finish_snapshot_failure(diagnostic.span),
        };
        self.latest_snapshot = Some(snapshot);
        self.host.diagnostic(diagnostic.clone());
        self.vm.cleanup_execution();
        self.pause_location = None;
        self.step_plan = None;
        self.state = ExecutionState::Faulted;
        RunOutcome::Faulted(diagnostic)
    }

    fn finish_cancelled(&mut self) -> RunOutcome {
        let (offset, span) = match self.vm.current_snapshot_point(self.document.eof_span()) {
            Ok(point) => point,
            Err(_) => return self.finish_snapshot_failure(self.document.eof_span()),
        };
        let request = snapshot::SnapshotRequest {
            reason: SnapshotReason::Cancelled,
            active_offset: Some(offset),
            current_span: span,
        };
        let snapshot = match self.vm.build_snapshot(request, &self.snapshot_limits) {
            Ok(snapshot) => snapshot,
            Err(_) => return self.finish_snapshot_failure(span),
        };
        self.latest_snapshot = Some(snapshot);
        self.vm.cleanup_execution();
        self.pause_location = None;
        self.step_plan = None;
        self.state = ExecutionState::Cancelled;
        RunOutcome::Cancelled
    }

    fn finish_snapshot_failure(&mut self, span: SourceSpan) -> RunOutcome {
        let diagnostic = Diagnostic {
            phase: DiagnosticPhase::Runtime,
            severity: DiagnosticSeverity::Error,
            code: "runtime.snapshot_unavailable".to_string(),
            message: "Debugger state is unavailable.".to_string(),
            span,
            frames: Vec::new(),
        };
        self.latest_snapshot = Some(snapshot::unavailable_snapshot(
            SnapshotReason::Faulted,
            span,
        ));
        self.host.diagnostic(diagnostic.clone());
        self.vm.cleanup_execution();
        self.pause_location = None;
        self.step_plan = None;
        self.state = ExecutionState::Faulted;
        RunOutcome::Faulted(diagnostic)
    }

    fn rejected(&self, operation: SessionOperation) -> RunOutcome {
        RunOutcome::Rejected(SessionError {
            operation,
            state: self.state,
        })
    }
}

impl<H: RuntimeHost> Drop for InterpreterSession<H> {
    fn drop(&mut self) {
        if matches!(self.state, ExecutionState::Running | ExecutionState::Paused) {
            self.vm.cleanup_execution();
        }
    }
}

#[derive(Default)]
struct CompilationHost {
    diagnostics: Vec<Diagnostic>,
}

impl RuntimeHost for CompilationHost {
    fn output(&mut self, _text: String) {}

    fn diagnostic(&mut self, value: Diagnostic) {
        self.diagnostics.push(value);
    }
}

fn select_point(points: &[DebugPoint], offset: usize) -> Option<DebugPoint> {
    points
        .iter()
        .filter(|point| point.offset == offset)
        .min_by_key(|point| match point.kind {
            DebugPointKind::FunctionEntry => 1,
            _ => 0,
        })
        .cloned()
}

pub(crate) fn semantic_point(points: &[DebugPoint], offset: usize) -> Option<DebugPoint> {
    select_point(points, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RecordingHost, SourceId};

    #[test]
    fn corrupt_pause_discovery_becomes_one_bounded_snapshot_fault() {
        let document = SourceDocument::new(SourceId(91), RevisionId(2), "corrupt.lox", "print 1;");
        let mut session = InterpreterSession::new(document, RecordingHost::default());
        let mut compilation = CompilationHost::default();
        assert!(
            session
                .vm
                .prepare(&session.document, &mut compilation)
                .is_ok()
        );
        session.state = ExecutionState::Running;
        let closure = session.vm.frames[0].closure;
        let function = unsafe { (*closure).function };
        unsafe { (*function).chunk.spans.pop() };

        let outcome = session.drive();

        let RunOutcome::Faulted(diagnostic) = outcome else {
            panic!("corrupt discovery did not become a fault")
        };
        assert_eq!(diagnostic.code, "runtime.snapshot_unavailable");
        assert_eq!(session.host.diagnostics().len(), 1);
        assert_eq!(session.host.diagnostics()[0], diagnostic);
        assert_eq!(session.state, ExecutionState::Faulted);
        assert_eq!(session.vm.frame_count, 0);
        assert_eq!(session.vm.stack_top, session.vm.stack.as_mut_ptr());
        let snapshot = session.snapshot().unwrap();
        assert_eq!(snapshot.reason, SnapshotReason::Faulted);
        assert!(snapshot.frames.is_empty());
        assert!(snapshot.frames_truncated);
        assert!(snapshot.globals.is_empty());
        assert!(snapshot.globals_truncated);
    }
}
