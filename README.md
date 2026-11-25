# siglus_rs

<img src="./icon/Icon.png" alt="icon" align="left" width="100" style="margin-right: 10px;" />

**siglus_rs** is an unofficial Rust implementation and multi-platform port of SiglusEngine.

This project is non-commercial and intended for research purposes.

Architectural decisions are recorded in [docs/adr](docs/adr/README.md).
Accepted decisions describe the intended design, not necessarily implemented features.

Siglus applies an **unofficial Unicode cell-spacing extension** automatically.
It requires no configuration file or activation step on any host.

The extension uses Unicode widths rather than a language whitelist. It handles
accented alphabetic text, font-measured Greek and Cyrillic, wide CJK, East Asian
ambiguous punctuation such as Japanese ellipses and dashes, and canonical
composition of decomposed characters in display runs. Newly handled alphabetic
characters use their resolved font advances, including fallback fonts, so wide
glyphs are not forced into overlapping half cells. Messages, names, ruby,
text objects, choices, backlog, reconstructed choices, and editbox geometry
share the policy. Editbox movement and deletion use grapheme boundaries.
Script strings and native UTF-16/width commands retain their original semantics.
Existing positioned save glyphs retain their coordinates.

This is the cell-spacing stage of [ADR 0001](docs/adr/0001-unicode-text-layout.md),
not complete Unicode typography. Contextual shaping, bidi, Unicode word/line
breaking, complex mark attachment, joined emoji, cluster-aware font fallback,
and lossless save reconstruction for future shaped layouts are still pending.
Font availability still determines glyph coverage.

<br clear="left"/>

## Example screenshots
* siglus_rs on macOS
![On Mac](./images/screenshot.png)

* siglus_rs on iOS
![On iOS](./images/ios-screenshot.png)

* siglus_rs on WebAssembly
![On Web](./images/screenshot-wasm.png)

* siglus_rs works on a wide range of platforms, including Windows, Linux, macOS, iOS, Android, and WebAssembly.

| Platform | Targets |
|---|---|
| Linux | x86_64, aarch64 |
| FreeBSD | x86_64 (manual builds) |
| Windows | x86_64, ARM64 |
| macOS | aarch64 app, x86_64 app, universal DMG app bundle |
| iOS | arm64 device, arm64 simulator, x86_64 simulator |
| Android | arm64-v8a, x86_64 |
| WebAssembly | wasm32-unknown-unknown |

## Pre-built binaries
* See preview releases on [GitHub Releases](https://github.com/xmoezzz/siglus_rs/releases)

Android APKs from non-fork pushes and all tag builds use a persistent signing
key. The build aligns the APK, signs it, and verifies its signature before
uploading the `siglus-android` artifact.
Repository secrets `ANDROID_KEY_BASE64` (base64-encoded keystore),
`ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`, and `ANDROID_KEYSTORE_PASSWORD` are
required. Missing signing configuration fails the Android job. Keep the same
key for future releases so installed apps can be updated.

Pull requests and branch pushes in forks only check compilation. They upload
`siglus-android-unsigned/app-release-unsigned.apk`, which is not directly
installable and is never published as a release or rolling pre-release. No
release signing secrets are required for these checks. Forks that publish
tagged releases must configure their own persistent signing key and secrets.

## Documentation Availability
* API documentation is available at [docs](https://xmoezzz.github.io/siglus_rs/)
* [PS Vita port roadmap](platform/vita/ROADMAP.md) — planned milestones and validation criteria; Vita support is not yet implemented.

## Run

```bash
cargo run --release -p siglus_scene_vm --bin siglus_engine -- --project-dir ~/Documents/siglus_rs-main/testcase
```

Desktop windows use `icon.png`, `icon.ico`, or the first readable `.ico` file
(in filename order) from the game directory, falling back to the Siglus icon.
The engine uses winit `0.31.0-beta.3` to send window icons directly through
`xdg_toplevel_icon_v1` on Wayland compositors that support it. No desktop entry
is generated. Each game retains a separate Wayland application ID.

## Community
If you want to join the development and discussion of this project, you can join the following Discord server:
* Discord: [https://discord.gg/g4rXucPZz3](https://discord.gg/g4rXucPZz3)
* Personally, I only able to speak English, Chinese, Japanese, and very limited French. 


## The `key.toml` configuration file
The `key.toml` file is used to specify different configuration options for siglus_rs. The file should be placed in the root directory of the game project.

### Resource decryption key
* This configuration key for this element is `key`. It is an array of 16 bytes (128 bits) that represents the secondary key.

* SiglusEngine games require a secondary key to decrypt protected resources.

siglus_rs can automatically brute-force the secondary key, but if you want specify the key manually, you can create a `key.toml` file in the game root directory.

There are several practical ways to obtain the key:

1. Static extraction, when the game executable is not encrypted or obfuscated (recommended). The general idea can be found in this repository:

   https://github.com/xmoezzz/siglus_static_key_tool

2. Dynamic extraction. The general idea can be found in this older repository:

   https://github.com/xmoezzz/SiglusExtract

3. Known-key databases maintained by some extractor tools.

Brute-force will be attempted in the following situations:
1. If the key is not specified in the `key.toml` file, siglus_rs will try to brute-force the key.
2. Users specify a wrong key in the `key.toml` file. siglus_rs will try to override the key.
3. If siglus_rs fails to save or overwrite the `key.toml` file, the engine will still execute.

Trial games may not require a secondary key, and in that case, you can specify all-zero key in the `key.toml` file. Here is an example of `key.toml`:

```toml
key = [
  0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x00, 0x00,
]
```

### String Encryption
* This configuration key for this element is `override_string_encryption`. The value of this key can be `xor`, `none`, or `mdl`. The default value is `xor`. 
* In very earlier versions of SiglusEngine, string encryption was not used. 
* However, in later versions (for the most cases), string encryption is enabled by default. 

Explanation of each value:
* `xor`: Enbales the string encryption. This is the default value even if the `override_string_encryption` key is not specified in the `key.toml` file.
* `none`: Disables the string encryption. If you are sure that the game does not use string encryption.
* `mdl`: Automatically detects the string encryption method by using the MDL approach (also see the paper: [https://arxiv.org/abs/cs/0312044](https://arxiv.org/abs/cs/0312044)). It does introduce a performance overhead, but IMO, it's minor.

## Claims
<p align="center">
  <img src="icon/crysirror.svg" width="160" height="160" alt="CrySirror logo">
</p>

This project belongs to `CrySirror` [project](https://github.com/xmoezzz/CrySirror), which is a unified engine backend for visual novels and 3D games.

1. Bug fixes and improvements for the siglus engine is acceptable in this repository.

2. If you want to support a new platform (e.g. game console), please create an issue or pull request in the `CrySirror` repository instead of this one.


## License
This project is licensed under the MPL-2.0 License. See [LICENSE](./LICENSE-MPL-2.0) for details.
