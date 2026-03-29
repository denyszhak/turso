use crate::{vdbe::insn::SubprogramBackpatch, LimboError, Result};
use std::sync::{Arc, OnceLock};

/// Internal safety belt for recursive FK action compilation.
///
/// This is intentionally separate from the user-visible trigger recursion limit,
/// which only governs runtime nested subprogram execution.
const MAX_FK_ACTION_COMPILE_DEPTH: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FkActionCompileKey {
    DeleteCascade(usize),
    DeleteSetNull(usize),
    DeleteSetDefault(usize),
    UpdateCascade(usize),
    UpdateSetNull(usize),
    UpdateSetDefault(usize),
}

#[derive(Debug, Clone)]
pub(crate) enum FkCompilationStart {
    CycleDetected(SubprogramBackpatch),
    Proceed,
}

#[derive(Debug, Clone)]
struct FkActionCompileFrame {
    key: FkActionCompileKey,
    backpatch: SubprogramBackpatch,
}

#[derive(Debug, Default)]
pub(crate) struct FkCompileContext {
    compiling_actions: Vec<FkActionCompileFrame>,
}

impl FkCompileContext {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn start_action_compilation(
        &mut self,
        key: FkActionCompileKey,
    ) -> Result<FkCompilationStart> {
        if let Some(frame) = self
            .compiling_actions
            .iter()
            .rev()
            .find(|frame| frame.key == key)
        {
            return Ok(FkCompilationStart::CycleDetected(frame.backpatch.clone()));
        }

        if self.compiling_actions.len() >= MAX_FK_ACTION_COMPILE_DEPTH {
            return Err(LimboError::ParseError(format!(
                "foreign key action graph is too deep to compile (>{MAX_FK_ACTION_COMPILE_DEPTH} nested subprograms)"
            )));
        }

        self.compiling_actions.push(FkActionCompileFrame {
            key,
            backpatch: Arc::new(OnceLock::new()),
        });

        Ok(FkCompilationStart::Proceed)
    }

    pub(crate) fn end_action_compilation(
        &mut self,
        expected_key: FkActionCompileKey,
    ) -> SubprogramBackpatch {
        let frame = self
            .compiling_actions
            .pop()
            .expect("FK action compilation stack underflow");

        assert_eq!(
            frame.key, expected_key,
            "FK action compilation stack corrupted"
        );

        frame.backpatch
    }
}
