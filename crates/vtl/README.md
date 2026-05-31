# coldstore-vtl

ColdStore 的虚拟磁带库 harness。该 crate 的目标不是在开发机上自动安装或启动 mhVTL，而是先把后续 TapeService 闭环开发需要的边界固化下来：

- 安全单测：不执行系统命令、不加载内核模块、不创建 `/dev/st*` / `/dev/sg*` / `/dev/sch*`。
- 命令封装：稳定暴露 `lsscsi`、`mtx`、`mt`、`sg3_utils`、`dd`、`vtlcmd` 的命令构造。
- 真实后端预留：在独立 VM/专用测试机安装 mhVTL 后，用 `SystemCommandRunner` 执行相同命令。
- 内存模拟器：提供 medium changer + tape drive 的 slot/barcode/load/unload/rewind/filemark/read/write 行为，供 phase-1/phase-2 单测使用。

## 模块分层

| 模块 | 职责 | 是否触碰宿主 |
|---|---|---|
| `model` | `TapeBarcode`、`ElementAddress`、`VirtualTape`、filemark/cursor 模型 | 否 |
| `interface` | `MediumChanger`、`TapeDrive`、`TapeInventory` trait | 否 |
| `simulator` | 纯内存 VTL，模拟 slot、drive、barcode、load/unload、rewind、filemark、read/write | 否 |
| `discover` | 解析 `lsscsi -g` 输出为 `ScsiInventory` | 否 |
| `command` | `CommandSpec`、`CommandRunner`、`SystemCommandRunner`、`RecordedCommandRunner` | 只有 `SystemCommandRunner` 会执行 |
| `mhvtl` | mhVTL/live 工具链命令构造和可选执行入口 | 构造命令不触碰；执行需显式 runner |

## 安全验证

```bash
cargo test -p coldstore-vtl --lib --tests
```

这只运行单测，不安装系统工具，不访问真实设备。

## 后续 live mhVTL 使用边界

live 环境必须放在专用 VM 或测试机中，不建议直接在当前开发宿主机执行：

1. 执行前人工阅读 `scripts/setup-mhvtl-env.sh`。
2. 只在专用 VM 中运行：
   ```bash
   scripts/setup-mhvtl-env.sh --execute --start-services
   ```
3. 验证设备发现：
   ```bash
   lsscsi -g
   ```
4. 用 `coldstore-vtl::mhvtl::MhvtlToolchain` 生成命令，再由上层测试 harness 决定是否执行。

## 当前已覆盖能力

- `lsscsi -g` 样例解析：识别 `mediumx` / `tape`，抽取 HCTL、vendor、product、revision、`/dev/schX`、`/dev/stX`、`/dev/nstX`、`/dev/sgX`。
- `mtx` 命令封装：status、transfer、load、unload。
- `mt` 命令封装：status、rewind、offline、weof、fsf。
- `sg3_utils` 命令封装：sg_inq、sg_turs、sg_logs、sg_modes。
- `dd` 命令封装：向 tape device 写入/读取数据。
- 内存 VTL：slot 插带、load/unload、drive 读写、rewind、filemark seek。
