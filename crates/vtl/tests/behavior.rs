use coldstore_vtl::command::{CommandSpec, RecordedCommandRunner};
use coldstore_vtl::discover::{parse_lsscsi, ScsiDeviceKind};
use coldstore_vtl::mhvtl::MhvtlToolchain;
use coldstore_vtl::model::{ElementAddress, TapeBarcode};
use coldstore_vtl::simulator::VirtualTapeLibrary;

const LSSCSI_SAMPLE: &str = r#"
[5:0:0:0]    mediumx STK      L700             0106  /dev/sch0  /dev/sg8
[5:0:1:0]    tape    IBM      ULT3580-TD5      0106  /dev/st4   /dev/sg10
[5:0:2:0]    tape    IBM      ULT3580-TD5      0106  /dev/st1   /dev/sg6
"#;

#[test]
fn parses_lsscsi_tape_and_medium_changer_devices() {
    let inventory = parse_lsscsi(LSSCSI_SAMPLE).expect("parse lsscsi output");

    let changers = inventory.medium_changers();
    assert_eq!(changers.len(), 1);
    assert_eq!(changers[0].kind, ScsiDeviceKind::MediumChanger);
    assert_eq!(changers[0].hctl, "5:0:0:0");
    assert_eq!(changers[0].vendor, "STK");
    assert_eq!(changers[0].product, "L700");
    assert_eq!(changers[0].primary_device.as_deref(), Some("/dev/sch0"));
    assert_eq!(changers[0].sg_device.as_deref(), Some("/dev/sg8"));

    let drives = inventory.tape_drives();
    assert_eq!(drives.len(), 2);
    assert_eq!(drives[0].kind, ScsiDeviceKind::TapeDrive);
    assert_eq!(drives[0].primary_device.as_deref(), Some("/dev/st4"));
    assert_eq!(drives[0].non_rewinding_device.as_deref(), Some("/dev/nst4"));
    assert_eq!(drives[0].sg_device.as_deref(), Some("/dev/sg10"));
}

#[test]
fn mhvtl_toolchain_exposes_stable_lsscsi_mtx_mt_and_sg_command_paths() {
    let tools = MhvtlToolchain::default();

    assert_eq!(
        tools.discover_command(),
        CommandSpec::new("lsscsi").arg("-g")
    );
    assert_eq!(
        tools.library_status_command("/dev/sg8"),
        CommandSpec::new("mtx").args(["-f", "/dev/sg8", "status"])
    );
    assert_eq!(
        tools.move_medium_command(
            "/dev/sg8",
            ElementAddress::slot(1),
            ElementAddress::drive(0)
        ),
        CommandSpec::new("mtx").args(["-f", "/dev/sg8", "transfer", "1", "0"])
    );
    assert_eq!(
        tools.load_command(
            "/dev/sg8",
            ElementAddress::slot(2),
            ElementAddress::drive(0)
        ),
        CommandSpec::new("mtx").args(["-f", "/dev/sg8", "load", "2", "0"])
    );
    assert_eq!(
        tools.unload_command(
            "/dev/sg8",
            ElementAddress::slot(2),
            ElementAddress::drive(0)
        ),
        CommandSpec::new("mtx").args(["-f", "/dev/sg8", "unload", "2", "0"])
    );
    assert_eq!(
        tools.rewind_command("/dev/nst0"),
        CommandSpec::new("mt").args(["-f", "/dev/nst0", "rewind"])
    );
    assert_eq!(
        tools.write_filemark_command("/dev/nst0", 2),
        CommandSpec::new("mt").args(["-f", "/dev/nst0", "weof", "2"])
    );
    assert_eq!(
        tools.seek_filemark_command("/dev/nst0", 1),
        CommandSpec::new("mt").args(["-f", "/dev/nst0", "fsf", "1"])
    );
    assert_eq!(
        tools.test_unit_ready_command("/dev/sg10"),
        CommandSpec::new("sg_turs").arg("/dev/sg10")
    );
    assert_eq!(
        tools.inquiry_command("/dev/sg10"),
        CommandSpec::new("sg_inq").arg("/dev/sg10")
    );
}

#[test]
fn mhvtl_discovery_uses_injected_runner_instead_of_host_commands() {
    let tools = MhvtlToolchain::default();
    let runner = RecordedCommandRunner::default();
    runner.push_stdout(LSSCSI_SAMPLE);

    let inventory = tools
        .discover(&runner)
        .expect("discover from recorded output");

    assert_eq!(inventory.tape_drives().len(), 2);
    assert_eq!(runner.commands(), vec![tools.discover_command()]);
}

#[test]
fn memory_vtl_models_changer_load_unload_and_tape_filemark_flow() {
    let mut vtl = VirtualTapeLibrary::new(3, 1);
    vtl.insert_tape(ElementAddress::slot(1), TapeBarcode::new("TAPE001"))
        .expect("insert tape");

    vtl.load(ElementAddress::slot(1), ElementAddress::drive(0))
        .expect("load tape");
    assert!(vtl.slot(ElementAddress::slot(1)).unwrap().is_empty());
    assert_eq!(
        vtl.drive(ElementAddress::drive(0))
            .unwrap()
            .loaded_barcode()
            .map(|b| b.as_str()),
        Some("TAPE001")
    );

    vtl.write(ElementAddress::drive(0), b"abc")
        .expect("write abc");
    vtl.write_filemark(ElementAddress::drive(0))
        .expect("write filemark");
    vtl.write(ElementAddress::drive(0), b"def")
        .expect("write def");

    vtl.rewind(ElementAddress::drive(0)).expect("rewind");
    assert_eq!(
        vtl.read(ElementAddress::drive(0), 3).expect("read abc"),
        b"abc"
    );
    vtl.seek_filemark(ElementAddress::drive(0), 1)
        .expect("seek filemark");
    assert_eq!(
        vtl.read(ElementAddress::drive(0), 3).expect("read def"),
        b"def"
    );

    vtl.unload(ElementAddress::drive(0), ElementAddress::slot(2))
        .expect("unload tape");
    assert!(vtl.drive(ElementAddress::drive(0)).unwrap().is_empty());
    assert_eq!(
        vtl.slot(ElementAddress::slot(2))
            .unwrap()
            .barcode()
            .map(|b| b.as_str()),
        Some("TAPE001")
    );
}
