# Dabao Console Out-of-Tree Build

This repo serves as a CI target and reference code for how to build an "out of tree" app for Xous.

"Out of tree" means only the application code is in the repository, and the xous-core kernel is a github reference.

> [!IMPORTANT]
> Applications and kernels are closely linked by design. A given 'kernel' can be thought of as a "distro" that contains a subset of system services; an 'app' will have dependencies on specific features in a given 'distro'. If your app isn't working, check that the loader and kernel on your device match the assumptions of your app!

All of the Xous dependencies relied upon by an app are encoded with git hash references to the Xous workspace. This effectively pins the app to a given version of Xous.

# Building the Stand-Alone ELF Binary

You can build the app with a command line like this:

`cargo build --release --target riscv32imac-unknown-xous-elf --features board-dabao --features bao1x --features utralib/bao1x`

The command line specifies the following:

- `--release` is needed to achieve the code density necessary to fit the app on the target device.
- `--target riscv32imac-unknown-xous-elf` causes the output binary to be in a machine code that is compatible with the Baochip-1x
- `--features bao1x --features utralib/bao1x` selects the Baochip-1x platform configuration. The first flag configures the OS-level crates, and the second flag configures the underlying UTRA hardware register set.

The result will be an ELF file in the `target/riscv32imac-unknown-xous-elf/release/` directory.

# Using the ELF Binary

A raw ELF file can't be loaded onto a dabao; it must first be converted into a UF2 file. This can be done by using `xous-tools`.

Run this once to install the tools:

`cargo install xous-tools`

Then, you can run this every time to create the .UF2 file:

`xous-app-uf2 --elf target/riscv32imac-unknown-xous-elf/release/dabao-console`

This will create a file called `apps.uf2` which you can then copy into a Dabao.

> [!NOTE]
> The loader & xous ABI version *must* match the version pointed to in the Cargo.toml file.

All apps are built against a Xous version that is specified like this:

`git = "https://github.com/betrusted-io/xous-core", rev = "6f71359d18f457855562e712d48034595de7c342"`

A Xous version compatible with the commit ref specified here must be loaded on the Dabao, otherwise, you will have unpredictable program behavior. At the moment Xous is under heavy development, so there isn't a release tag that's guaranteed to work - it's recommended to either pull a Xous image from the [latest CI](https://ci.betrusted.io/latest-ci/baochip/dabao/) build, or preferably, to build an image using the `xous-core` repository.