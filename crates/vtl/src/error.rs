use crate::command::CommandSpec;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, VtlError>;

#[derive(Debug, Error)]
pub enum VtlError {
    #[error("invalid SCSI/lsscsi line: {0}")]
    InvalidLsscsiLine(String),

    #[error("invalid element address: {0}")]
    InvalidElementAddress(String),

    #[error("slot {0} does not exist")]
    SlotOutOfRange(u32),

    #[error("drive {0} does not exist")]
    DriveOutOfRange(u32),

    #[error("slot {0} is empty")]
    SlotEmpty(u32),

    #[error("slot {0} is already occupied")]
    SlotOccupied(u32),

    #[error("drive {0} is empty")]
    DriveEmpty(u32),

    #[error("drive {0} is already occupied")]
    DriveOccupied(u32),

    #[error("expected {expected} element, got {actual}")]
    WrongElement {
        expected: &'static str,
        actual: String,
    },

    #[error("filemark not found before end of tape")]
    FilemarkNotFound,

    #[error("command failed: {command}; status={status}; stderr={stderr}")]
    CommandFailed {
        command: CommandSpec,
        status: i32,
        stderr: String,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
