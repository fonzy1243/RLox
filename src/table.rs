use std::alloc::{Layout, dealloc};

use crate::object::ObjString;
use crate::value::Value;

const TABLE_MAX_LOAD: f64 = 0.75; // TODO: Benchmark best max load

#[derive(Clone, Copy)]
pub struct Entry {
    pub key: *mut ObjString,
    pub value: Value,
}

pub struct Table {
    pub count: usize,
    pub capacity: usize,
    pub entries: *mut Entry,
}

impl Table {
    pub fn new() -> Self {
        Self {
            count: 0,
            capacity: 0,
            entries: std::ptr::null_mut(),
        }
    }

    pub fn set(&mut self, key: *mut ObjString, value: Value) -> bool {
        if (self.count + 1) as f64 > (self.capacity as f64) * TABLE_MAX_LOAD {
            let new_capacity = self.grow_capacity(self.capacity);
            self.adjust_capacity(new_capacity);
        }

        let entry = find_entry(self.entries, self.capacity, key);

        let is_new_key = unsafe { (*entry).key.is_null() };

        if is_new_key && unsafe { (*entry).value.is_nil() } {
            self.count += 1;
        }

        unsafe {
            (*entry).key = key;
            (*entry).value = value;
        }

        is_new_key
    }

    fn grow_capacity(&self, capacity: usize) -> usize {
        if capacity < 8 { 8 } else { capacity * 2 }
    }

    fn adjust_capacity(&mut self, capacity: usize) {
        // Allocate entries
        let layout = std::alloc::Layout::array::<Entry>(capacity).unwrap();
        let entries = unsafe {
            let ptr = std::alloc::alloc(layout) as *mut Entry;
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }

            // Init to empty
            for i in 0..capacity {
                let entry = ptr.add(i);
                (*entry).key = std::ptr::null_mut();
                (*entry).value = Value::Nil;
            }
            ptr
        };

        self.count = 0;

        for i in 0..self.capacity {
            unsafe {
                let entry = self.entries.add(i);

                if (*entry).key.is_null() {
                    continue;
                }

                let dest = find_entry(entries, capacity, (*entry).key);
                (*dest).key = (*entry).key;
                (*dest).value = (*entry).value;

                self.count += 1;
            }
        }

        if !self.entries.is_null() {
            unsafe {
                let old_layout = std::alloc::Layout::array::<Entry>(self.capacity).unwrap();
                std::alloc::dealloc(self.entries as *mut u8, old_layout);
            }
        }

        self.entries = entries;
        self.capacity = capacity;
    }

    pub fn add_all(&mut self, from: &Table) {
        for i in 0..from.capacity {
            unsafe {
                let entry = from.entries.add(i);
                if !(*entry).key.is_null() {
                    self.set((*entry).key, (*entry).value);
                }
            }
        }
    }

    pub fn find_string(&self, chars: &str, hash: u32) -> Option<*mut ObjString> {
        if self.count == 0 {
            return None;
        }

        let mut index = (hash as usize) % self.capacity;

        loop {
            let entry = unsafe { self.entries.add(index) };
            let key = unsafe { (*entry).key };

            if key.is_null() {
                if unsafe { (*entry).value.is_nil() } {
                    return None;
                }
            } else {
                unsafe {
                    if (*key).length == chars.len() && (*key).hash == hash {
                        if ObjString::as_str(key) == chars {
                            return Some(key);
                        }
                    }
                }
            }

            index = (index + 1) % self.capacity;
        }
    }

    pub fn get(&self, key: *mut ObjString) -> Option<Value> {
        if self.count == 0 {
            return None;
        }

        let entry = find_entry(self.entries, self.capacity, key);

        unsafe {
            if (*entry).key.is_null() {
                None
            } else {
                Some((*entry).value)
            }
        }
    }

    pub fn delete(&mut self, key: *mut ObjString) -> bool {
        if self.count == 0 {
            return false;
        }

        let entry = find_entry(self.entries, self.capacity, key);

        unsafe {
            if (*entry).key.is_null() {
                return false;
            }

            (*entry).key = std::ptr::null_mut();
            (*entry).value = Value::Bool(true);

            true
        }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if !self.entries.is_null() {
            unsafe {
                let layout = Layout::array::<Entry>(self.capacity).unwrap();
                dealloc(self.entries as *mut u8, layout);
            }

            self.count = 0;
            self.capacity = 0;
            self.entries = std::ptr::null_mut();
        }
    }
}

fn find_entry(entries: *mut Entry, capacity: usize, key: *mut ObjString) -> *mut Entry {
    let hash = unsafe { (*key).hash };
    let mut index = (hash as usize) % capacity;
    let mut tombstone: *mut Entry = std::ptr::null_mut();

    loop {
        let entry = unsafe { entries.add(index) };
        let entry_key = unsafe { (*entry).key };

        if entry_key.is_null() {
            let is_empty = unsafe { (*entry).value.is_nil() };

            if is_empty {
                return if !tombstone.is_null() {
                    tombstone
                } else {
                    entry
                };
            } else {
                if tombstone.is_null() {
                    tombstone = entry;
                }
            }
        } else if entry_key == key {
            return entry;
        }

        index = (index + 1) % capacity;
    }
}
