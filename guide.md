# Dend-p2p 编译指南 (macOS Darwin)

本指南介绍如何在 macOS 环境下为 macOS、Windows 和 Linux 编译 `dend-p2p`。

因为涉及不同操作系统（C 库差异），推荐使用 `cross` 工具进行交叉编译。

## 1. 安装 Rust 和 Docker

首先确保安装了 Rust 和 Docker（Docker Desktop 或 OrbStack）。

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 验证
cargo --version
```

## 2. 安装 `cross`

`cross` 是一个基于 Docker 的零配置交叉编译工具。

```bash
cargo install cross
```

## 3. 编译

### 3.1 编译 macOS 版本 (本地)

直接使用 `cargo build` 即可。

```bash
cargo build --release --target x86_64-apple-darwin
# 或者 M1/M2/M3
cargo build --release --target aarch64-apple-darwin
```

产物位置: `target/release/dend-p2p`

### 3.2 编译 Linux 版本 (x86_64)

使用 `cross` 编译静态链接的 musl 版本，兼容性最好。

**注意**: 如果你在 macOS (Apple Silicon) 上遇到 `toolchain ... may not be able to run on this system` 错误，请先尝试手动添加目标：

```bash
rustup target add x86_64-unknown-linux-musl
```

然后运行编译命令：

```bash
cross build --release --target x86_64-unknown-linux-musl
```

如果 `cross` 仍然报错，你可以直接使用 Docker 进行编译 (作为备选方案):

```bash
# 使用 rust-musl-cross 镜像
docker run --rm -v "$(pwd)":/home/rust/src \
  -w /home/rust/src \
  messense/rust-musl-cross:x86_64-musl \
  cargo build --release
```

产物位置: 
- `cross`: `target/x86_64-unknown-linux-musl/release/dend-p2p`
- `docker`: `target/x86_64-unknown-linux-musl/release/dend-p2p`

### 3.3 编译 Windows 版本 (x86_64)

使用 `cross` 编译 mingw 版本。

```bash
cross build --release --target x86_64-pc-windows-gnu
```

产物位置: `target/x86_64-pc-windows-gnu/release/dend-p2p.exe`

## 注意事项

- **Wintun**: Windows 版本运行时需要 `wintun.dll`。请从 [wintun.net](https://www.wintun.net/) 下载，并将其与 `dend-p2p.exe` 放在同一目录。
- **macOS/Linux**: 运行时需要 `sudo` 权限来创建网络设备。

---
## 分发包结构建议

建议将编译产物和配置文件打包：

**Windows:**
```
dend-p2p-win/
  ├── dend-p2p.exe
  ├── wintun.dll  (必须手动放入)
  ├── config.toml (修改为 client 模式)
  └── README.txt
```

**Linux:**
```
dend-p2p-linux/
  ├── dend-p2p
  └── config.toml (修改为 server/client 模式)
```
