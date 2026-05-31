use crate::error::{Result, VtlError};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::process::Command;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CommandSpec {
    program: String,
    args: Vec<String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self) -> &[String] {
        &self.args
    }

    pub fn argv(&self) -> Vec<&str> {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect()
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", shell_quote(&self.program))?;
        for arg in &self.args {
            write!(f, " {}", shell_quote(arg))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }
}

pub trait CommandRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput>;

    fn run_checked(&self, command: &CommandSpec) -> Result<CommandOutput> {
        let output = self.run(command)?;
        if output.status == 0 {
            Ok(output)
        } else {
            Err(VtlError::CommandFailed {
                command: command.clone(),
                status: output.status,
                stderr: output.stderr,
            })
        }
    }
}

#[derive(Debug, Default, Copy, Clone)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput> {
        let output = Command::new(command.program())
            .args(command.arguments())
            .output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, Default)]
pub struct RecordedCommandRunner {
    commands: RefCell<Vec<CommandSpec>>,
    responses: RefCell<VecDeque<CommandOutput>>,
}

impl RecordedCommandRunner {
    pub fn push_response(&self, response: CommandOutput) {
        self.responses.borrow_mut().push_back(response);
    }

    pub fn push_stdout(&self, stdout: impl Into<String>) {
        self.push_response(CommandOutput::success(stdout));
    }

    pub fn commands(&self) -> Vec<CommandSpec> {
        self.commands.borrow().clone()
    }
}

impl CommandRunner for RecordedCommandRunner {
    fn run(&self, command: &CommandSpec) -> Result<CommandOutput> {
        self.commands.borrow_mut().push(command.clone());
        Ok(self.responses.borrow_mut().pop_front().unwrap_or_default())
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace("'", "'\\''"))
    }
}
