// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `rtm extract-worker` process (ADR-0006, FR-EXT-06): sandbox itself, read one
//! request from stdin, write one response to stdout, exit.
//!
//! Limits are applied before any untrusted input is read. This is the first layer;
//! the server adds a wall-clock kill, a cleared environment and bounded output, and
//! landlock and seccomp filters are to follow.

use std::io::{self, Write};

use rlimit::Resource;

use crate::protocol::{handle, read_request};

/// Resource limits for the worker (ADR-0006 defaults).
#[derive(Debug, Clone, Copy)]
pub struct WorkerLimits {
    /// Address space, bytes.
    pub memory: u64,
    /// CPU time, seconds; the kernel kills the process when it's used up.
    pub cpu_seconds: u64,
    /// Largest source accepted on stdin.
    pub max_source: usize,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            memory: 512 * 1024 * 1024,
            cpu_seconds: 20,
            max_source: 10 * 1024 * 1024,
        }
    }
}

/// Apply resource limits to this process. Both soft and hard limits are set, so the
/// worker can't raise them again.
pub fn limit_self(limits: &WorkerLimits) -> io::Result<()> {
    Resource::AS.set(limits.memory, limits.memory)?;
    // SIGXCPU at the soft limit; SIGKILL a second later if that's ignored.
    Resource::CPU.set(limits.cpu_seconds, limits.cpu_seconds + 1)?;
    // No files may be written (stdout and stderr are pipes, which this doesn't cover).
    Resource::FSIZE.set(0, 0)?;
    // No core dumps of untrusted content.
    Resource::CORE.set(0, 0)?;
    // A handful of descriptors: stdin, stdout, stderr and a little slack.
    Resource::NOFILE.set(16, 16)?;
    Ok(())
}

/// Run one extraction over stdin and stdout. Returns the process exit code.
pub fn run(limits: &WorkerLimits) -> u8 {
    if let Err(e) = limit_self(limits) {
        eprintln!("extract-worker: could not apply limits: {e}");
        return 70;
    }
    let (header, source) = match read_request(io::stdin().lock(), limits.max_source) {
        Ok(request) => request,
        Err(e) => {
            eprintln!("extract-worker: bad request: {e}");
            return 65;
        }
    };
    let response = handle(header, source);
    let mut out = io::stdout().lock();
    match serde_json::to_writer(&mut out, &response)
        .map_err(io::Error::from)
        .and_then(|()| out.flush())
    {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("extract-worker: could not write response: {e}");
            74
        }
    }
}
