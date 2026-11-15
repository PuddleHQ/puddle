// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Mutex;

use super::{LoginEntry, LoginError};

/// A list in memory, counting what was changed.
#[derive(Debug, Default)]
pub(crate) struct Fake {
    value: Mutex<Option<String>>,
    changes: Mutex<u32>,
}

impl Fake {
    pub(crate) fn holding(command: &str) -> Self {
        Self {
            value: Mutex::new(Some(command.to_owned())),
            changes: Mutex::new(0),
        }
    }

    pub(crate) fn now(&self) -> Option<String> {
        self.value.lock().unwrap().clone()
    }

    pub(crate) fn changes(&self) -> u32 {
        *self.changes.lock().unwrap()
    }
}

impl LoginEntry for Fake {
    fn command(&self) -> Result<Option<String>, LoginError> {
        Ok(self.now())
    }

    fn set(&self, command: &str) -> Result<(), LoginError> {
        *self.value.lock().unwrap() = Some(command.to_owned());
        *self.changes.lock().unwrap() += 1;
        Ok(())
    }

    fn clear(&self) -> Result<(), LoginError> {
        *self.value.lock().unwrap() = None;
        *self.changes.lock().unwrap() += 1;
        Ok(())
    }
}
