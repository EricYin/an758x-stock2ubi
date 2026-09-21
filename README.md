# AN758x-Stock2UBI

Install U-Boot recovery from the stock system without opening the device.
Back up partitions and upload the boot images through the Web page; the device
reboots into recovery when flashing finishes.

在原厂系统里刷入 U-Boot 恢复引导，无需拆机。打开网页备份分区并上传引导镜像，
写入完成后设备会自动重启到恢复页面。

## Use / 使用

1. Upload `an758x-stock2ubi` to `/tmp` on the device.<br>
   把 `an758x-stock2ubi` 上传到设备的 `/tmp`。
2. Make it executable:<br>
   添加执行权限：

   ```sh
   chmod +x /tmp/an758x-stock2ubi
   ```

3. Run it as root:<br>
   以 root 身份运行：

   ```sh
   /tmp/an758x-stock2ubi
   ```

Open `http://<device-ip>:3333/`, back up the partitions you need, and upload the matching `preloader.bin` or `firstblock.bin` together with the `bl31-u-boot.fip` from [AN758x U-Boot](https://github.com/pbs05/uboot-an758x).
After reboot, open `http://192.168.0.1/` to continue installation. The recovery page may take about one minute to appear on the first boot.

打开 `http://<设备IP>:3333/`，备份需要保留的分区，再上传对应机型的`preloader.bin` 或 `firstblock.bin`，以及[AN758x U-Boot](https://github.com/pbs05/uboot-an758x) 的`bl31-u-boot.fip`。
重启后访问 `http://192.168.0.1/` 继续安装；首次进入恢复页面可能需要约一分钟。

The default port is `3333`; use `--listen IP:PORT` to change it. Startup checks
for kernel release `5.4.55`; `--ignore-kernel-version` skips that check.<br>
默认端口为 `3333`，可用 `--listen IP:PORT` 修改。启动时检查内核 release 为
`5.4.55`；`--ignore-kernel-version` 可跳过该检查。

## Cross-build / 交叉编译

Cross-compile for AArch64 Linux with Rust. The included `an758x_mtd_rw.ko` is
embedded automatically.

使用 Rust 交叉编译 AArch64 Linux 程序。仓库中的 `an758x_mtd_rw.ko` 会自动嵌入。

```sh
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl
```

Output / 产物：`target/aarch64-unknown-linux-musl/release/an758x-stock2ubi`
