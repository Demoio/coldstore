.PHONY: build fmt clippy run clean test help

# 默认目标
.DEFAULT_GOAL := help

# 项目名称
PROJECT := coldstore

# 构建模式（release 或 debug）
BUILD_MODE ?= release

# 运行参数
RUN_ARGS ?=

# 构建项目
build:
	@echo "构建项目 ($(BUILD_MODE))..."
	@if [ "$(BUILD_MODE)" = "release" ]; then \
		cargo build --release; \
	else \
		cargo build; \
	fi

# 快速构建（debug 模式）
build-debug:
	@$(MAKE) BUILD_MODE=debug build

# 格式化代码
fmt:
	@echo "格式化代码..."
	@cargo fmt --all
	@echo "代码格式化完成"

# 检查代码格式
fmt-check:
	@echo "检查代码格式..."
	@cargo fmt --all -- --check

# 运行 Clippy 代码检查
clippy:
	@echo "运行 Clippy 代码检查..."
	@cargo clippy --all-targets --all-features -- -D warnings
	@echo "Clippy 检查完成"

# 运行 Clippy（允许警告）
clippy-allow:
	@echo "运行 Clippy 代码检查（允许警告）..."
	@cargo clippy --all-targets --all-features

# 运行项目
run:
	@echo "运行项目..."
	@cargo run --release -- $(RUN_ARGS)

# 运行项目（debug 模式）
run-debug:
	@echo "运行项目（debug 模式）..."
	@cargo run -- $(RUN_ARGS)

# 清理构建产物
clean:
	@echo "清理构建产物..."
	@cargo clean
	@echo "清理完成"

# 运行测试
test:
	@echo "运行测试..."
	@cargo test --all-features
	@echo "测试完成"

# 运行测试（显示输出）
test-verbose:
	@echo "运行测试（显示输出）..."
	@cargo test --all-features -- --nocapture

# 检查代码（不构建）
check:
	@echo "检查代码..."
	@cargo check --all-targets --all-features
	@echo "检查完成"

# 完整检查（格式化 + Clippy + 测试）
check-all: fmt-check clippy test
	@echo "所有检查完成"

# 开发前检查（格式化 + Clippy）
dev-check: fmt-check clippy
	@echo "开发检查完成"

# 安装开发工具
install-tools:
	@echo "安装开发工具..."
	@rustup component add rustfmt clippy
	@echo "开发工具安装完成"

# 更新依赖
update:
	@echo "更新依赖..."
	@cargo update
	@echo "依赖更新完成"

# 显示帮助信息
help:
	@echo "ColdStore 项目 Makefile"
	@echo ""
	@echo "可用目标："
	@echo "  build          - 构建项目（release 模式）"
	@echo "  build-debug    - 构建项目（debug 模式）"
	@echo "  fmt            - 格式化代码"
	@echo "  fmt-check      - 检查代码格式"
	@echo "  clippy         - 运行 Clippy 代码检查（严格模式）"
	@echo "  clippy-allow   - 运行 Clippy 代码检查（允许警告）"
	@echo "  run            - 运行项目（release 模式）"
	@echo "  run-debug      - 运行项目（debug 模式）"
	@echo "  clean          - 清理构建产物"
	@echo "  test           - 运行测试"
	@echo "  test-verbose   - 运行测试（显示输出）"
	@echo "  check          - 检查代码（不构建）"
	@echo "  check-all      - 完整检查（格式化 + Clippy + 测试）"
	@echo "  dev-check      - 开发前检查（格式化 + Clippy）"
	@echo "  install-tools  - 安装开发工具（rustfmt, clippy）"
	@echo "  update         - 更新依赖"
	@echo "  help           - 显示此帮助信息"
	@echo ""
	@echo "示例："
	@echo "  make build              # 构建 release 版本"
	@echo "  make build-debug        # 构建 debug 版本"
	@echo "  make fmt                # 格式化代码"
	@echo "  make clippy             # 运行 Clippy 检查"
	@echo "  make run                # 运行项目"
	@echo "  make run RUN_ARGS='--help'  # 带参数运行"
	@echo "  make check-all          # 运行所有检查"
