use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::{
    DebugPoint, DebugPointKind, Diagnostic, DiagnosticPhase, DiagnosticSeverity, RevisionId,
    RuntimeHost, SourceDocument, SourceId, SourceSpan, vm,
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
}

impl<H: RuntimeHost> InterpreterSession<H> {
    pub fn new(document: SourceDocument, host: H) -> Self {
        Self {
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
        }
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

        let mut forwarding = ForwardingHost {
            inner: &mut self.host,
            first_diagnostic: None,
        };
        match self.vm.prepare(&self.document, &mut forwarding) {
            Ok(()) => {}
            Err(vm::StartError::Compile) => {
                let had_diagnostic = forwarding.first_diagnostic.is_some();
                let diagnostic = forwarding
                    .first_diagnostic
                    .take()
                    .unwrap_or_else(|| Diagnostic {
                        phase: DiagnosticPhase::Compiler,
                        severity: DiagnosticSeverity::Error,
                        code: "compiler.error".to_string(),
                        message: "Compilation failed without a diagnostic.".to_string(),
                        span: self.document.eof_span(),
                        frames: Vec::new(),
                    });
                if !had_diagnostic {
                    forwarding.inner.diagnostic(diagnostic.clone());
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

            if let Some((activation_id, point)) = self.vm.current_semantic_point() {
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

                    let reason = if self.control.acknowledge_pause() {
                        Some(PauseReason::Explicit)
                    } else if self.initial_debug_pause {
                        self.initial_debug_pause = false;
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
        let diagnostic = self
            .vm
            .diagnostic_for_fault(fault, self.document.eof_span());
        self.host.diagnostic(diagnostic.clone());
        self.vm.cleanup_execution();
        self.pause_location = None;
        self.step_plan = None;
        self.state = ExecutionState::Faulted;
        RunOutcome::Faulted(diagnostic)
    }

    fn finish_cancelled(&mut self) -> RunOutcome {
        self.vm.cleanup_execution();
        self.pause_location = None;
        self.step_plan = None;
        self.state = ExecutionState::Cancelled;
        RunOutcome::Cancelled
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

struct ForwardingHost<'a, H> {
    inner: &'a mut H,
    first_diagnostic: Option<Diagnostic>,
}

impl<H: RuntimeHost> RuntimeHost for ForwardingHost<'_, H> {
    fn output(&mut self, text: String) {
        self.inner.output(text);
    }

    fn diagnostic(&mut self, value: Diagnostic) {
        if self.first_diagnostic.is_none() {
            self.first_diagnostic = Some(value.clone());
        }
        self.inner.diagnostic(value);
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
