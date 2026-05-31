use crate::command::{CommandOutput, CommandRunner, CommandSpec};
use crate::discover::{parse_lsscsi, ScsiInventory};
use crate::error::Result;
use crate::model::ElementAddress;
use std::path::Path;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ToolPaths {
    pub lsscsi: String,
    pub mtx: String,
    pub mt: String,
    pub dd: String,
    pub sg_inq: String,
    pub sg_turs: String,
    pub sg_logs: String,
    pub sg_modes: String,
    pub vtlcmd: String,
}

impl Default for ToolPaths {
    fn default() -> Self {
        Self {
            lsscsi: "lsscsi".into(),
            mtx: "mtx".into(),
            mt: "mt".into(),
            dd: "dd".into(),
            sg_inq: "sg_inq".into(),
            sg_turs: "sg_turs".into(),
            sg_logs: "sg_logs".into(),
            sg_modes: "sg_modes".into(),
            vtlcmd: "vtlcmd".into(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct MhvtlToolchain {
    paths: ToolPaths,
}

impl MhvtlToolchain {
    pub fn new(paths: ToolPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &ToolPaths {
        &self.paths
    }

    pub fn discover_command(&self) -> CommandSpec {
        CommandSpec::new(self.paths.lsscsi.clone()).arg("-g")
    }

    pub fn library_status_command(&self, changer_sg: &str) -> CommandSpec {
        CommandSpec::new(self.paths.mtx.clone()).args(["-f", changer_sg, "status"])
    }

    pub fn move_medium_command(
        &self,
        changer_sg: &str,
        from: ElementAddress,
        to: ElementAddress,
    ) -> CommandSpec {
        CommandSpec::new(self.paths.mtx.clone()).args([
            "-f".to_string(),
            changer_sg.to_string(),
            "transfer".to_string(),
            from.mtx_address().to_string(),
            to.mtx_address().to_string(),
        ])
    }

    pub fn load_command(
        &self,
        changer_sg: &str,
        slot: ElementAddress,
        drive: ElementAddress,
    ) -> CommandSpec {
        CommandSpec::new(self.paths.mtx.clone()).args([
            "-f".to_string(),
            changer_sg.to_string(),
            "load".to_string(),
            slot.mtx_address().to_string(),
            drive.mtx_address().to_string(),
        ])
    }

    pub fn unload_command(
        &self,
        changer_sg: &str,
        slot: ElementAddress,
        drive: ElementAddress,
    ) -> CommandSpec {
        CommandSpec::new(self.paths.mtx.clone()).args([
            "-f".to_string(),
            changer_sg.to_string(),
            "unload".to_string(),
            slot.mtx_address().to_string(),
            drive.mtx_address().to_string(),
        ])
    }

    pub fn tape_status_command(&self, tape_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.mt.clone()).args(["-f", tape_device, "status"])
    }

    pub fn rewind_command(&self, tape_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.mt.clone()).args(["-f", tape_device, "rewind"])
    }

    pub fn offline_command(&self, tape_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.mt.clone()).args(["-f", tape_device, "offline"])
    }

    pub fn write_filemark_command(&self, tape_device: &str, count: u32) -> CommandSpec {
        CommandSpec::new(self.paths.mt.clone()).args([
            "-f".to_string(),
            tape_device.to_string(),
            "weof".to_string(),
            count.to_string(),
        ])
    }

    pub fn seek_filemark_command(&self, tape_device: &str, count: u32) -> CommandSpec {
        CommandSpec::new(self.paths.mt.clone()).args([
            "-f".to_string(),
            tape_device.to_string(),
            "fsf".to_string(),
            count.to_string(),
        ])
    }

    pub fn write_tape_command(
        &self,
        input_path: impl AsRef<Path>,
        tape_device: &str,
        block_size: u32,
    ) -> CommandSpec {
        CommandSpec::new(self.paths.dd.clone()).args([
            format!("if={}", input_path.as_ref().display()),
            format!("of={tape_device}"),
            format!("bs={block_size}"),
            "status=none".to_string(),
        ])
    }

    pub fn read_tape_command(
        &self,
        tape_device: &str,
        output_path: impl AsRef<Path>,
        block_size: u32,
        count: u32,
    ) -> CommandSpec {
        CommandSpec::new(self.paths.dd.clone()).args([
            format!("if={tape_device}"),
            format!("of={}", output_path.as_ref().display()),
            format!("bs={block_size}"),
            format!("count={count}"),
            "iflag=fullblock".to_string(),
            "status=none".to_string(),
        ])
    }

    pub fn inquiry_command(&self, sg_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.sg_inq.clone()).arg(sg_device)
    }

    pub fn test_unit_ready_command(&self, sg_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.sg_turs.clone()).arg(sg_device)
    }

    pub fn sg_logs_command(&self, sg_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.sg_logs.clone()).arg(sg_device)
    }

    pub fn sg_modes_command(&self, sg_device: &str) -> CommandSpec {
        CommandSpec::new(self.paths.sg_modes.clone()).arg(sg_device)
    }

    pub fn vtlcmd_command<I, S>(&self, args: I) -> CommandSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        CommandSpec::new(self.paths.vtlcmd.clone()).args(args)
    }

    pub fn discover(&self, runner: &dyn CommandRunner) -> Result<ScsiInventory> {
        let output = runner.run_checked(&self.discover_command())?;
        parse_lsscsi(&output.stdout)
    }

    pub fn run_library_status(
        &self,
        runner: &dyn CommandRunner,
        changer_sg: &str,
    ) -> Result<CommandOutput> {
        runner.run_checked(&self.library_status_command(changer_sg))
    }

    pub fn run_test_unit_ready(
        &self,
        runner: &dyn CommandRunner,
        sg_device: &str,
    ) -> Result<CommandOutput> {
        runner.run_checked(&self.test_unit_ready_command(sg_device))
    }
}
