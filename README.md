<div align="center">
	<sup>Special thanks to:</sup><br>

| <a href="https://go.warp.dev/yazi" target="_blank"><img alt="Warp sponsorship" width=350 src="https://github.com/warpdotdev/brand-assets/blob/main/Github/Sponsor/Warp-Github-LG-02.png"><br><b>Warp, built for coding with multiple AI agents</b><br><sup>Available for macOS, Linux and Windows</sup></a> |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |

</div>

## Yazi - ⚡️ Blazing Fast Terminal File Manager

Yazi (means "duck") is a terminal file manager written in Rust, based on non-blocking async I/O. It aims to provide an efficient, user-friendly, and customizable file management experience.

💡 A new article explaining its internal workings: [Why is Yazi Fast?](https://yazi-rs.github.io/blog/why-is-yazi-fast)

- 🚀 **Full Asynchronous Support**: All I/O operations are asynchronous, CPU tasks are spread across multiple threads, making the most of available resources.
- 💪 **Powerful Async Task Scheduling and Management**: Provides real-time progress updates, task cancellation, and internal task priority assignment.
- 🖼️ **Built-in Support for Multiple Image Protocols**: Also integrated with Überzug++ and Chafa, covering almost all terminals.
- 🌟 **Built-in Code Highlighting and Image Decoding**: Combined with the pre-loading mechanism, greatly accelerates image and normal file loading.
- 🔌 **Concurrent Plugin System**: UI plugins (rewriting most of the UI), functional plugins, custom previewer/preloader/spotter/fetcher; Just some pieces of Lua.
- ☁️ **Virtual Filesystem**: Remote file management, custom VFS provider, custom search engines.
- 📡 **Data Distribution Service**: Built on a client-server architecture (no additional server process required), integrated with a Lua-based publish-subscribe model, achieving cross-instance communication and state persistence.
- 📦 **Package Manager**: Install plugins and themes with one command, keeping them up-to-date, or pin them to a specific version.
- 🧰 Integration with ripgrep, fd, fzf, zoxide
- 💫 Vim-like input/pick/confirm/which/notify component, auto-completion for cd paths
- 🏷️ Multi-Tab Support, Cross-directory selection, Scrollable Preview (for videos, PDFs, archives, code, directories, etc.)
- 🔄 Bulk Rename/Create, Archive Extraction, Visual Mode, File Chooser, [Git Integration](https://github.com/yazi-rs/plugins/tree/main/git.yazi), [Mount Manager](https://github.com/yazi-rs/plugins/tree/main/mount.yazi)
- 🎨 Theme System, Mouse Support, [Drag and Drop](https://yazi-rs.github.io/docs/dnd), Trash Bin, Custom Layouts, CSI u, OSC 52, CSI 2031
- ... and more!

https://github.com/sxyazi/yazi/assets/17523360/92ff23fa-0cd5-4f04-b387-894c12265cc7

## Project status

Public beta, can be used as a daily driver.

Yazi is currently in heavy development, expect breaking changes.

## Documentation

- Usage: https://yazi-rs.github.io/docs/installation
- Features: https://yazi-rs.github.io/features

## `custom` 分支：输入法切换

前台导航、输入框 Normal 模式使用英文；输入框 Insert/Replace（含帮助搜索和插件输入）恢复进入命令态前的系统输入源。退出、失焦及交给外部程序时尝试恢复；前台命令态每约 100ms 检查用户手动切换的来源。切换失败会提示，不能保证 SIGKILL、系统崩溃或输入法服务不可用时恢复。

macOS 依赖 `/opt/homebrew/Cellar/macism/3.1.1/bin/macism`，英文源为 `com.apple.keylayout.ABC`；只恢复鼠须管的系统 source ID，不保存其内部 `ascii_mode`。Linux 本地图形会话优先使用 `fcitx5-remote`，其次 `fcitx-remote`；Fcitx5 恢复方法名及激活状态，Fcitx4 只能恢复激活状态。SSH、无本地图形会话或不可用的输入法服务不更改输入源。

## Discussion

- Discord Server (English mainly): https://discord.gg/qfADduSdJu
- Telegram Group (Chinese mainly): https://t.me/yazi_rs

## Image Preview

| Platform                                                                     | Protocol                               | Support                                |
| ---------------------------------------------------------------------------- | -------------------------------------- | -------------------------------------- |
| [kitty](https://github.com/kovidgoyal/kitty) (>= 0.28.0)                     | [Kitty unicode placeholders][kgp]      | ✅ Built-in                            |
| [iTerm2](https://iterm2.com)                                                 | [Inline images protocol][iip]          | ✅ Built-in                            |
| [WezTerm](https://github.com/wez/wezterm)                                    | [Inline images protocol][iip]          | ✅ Built-in                            |
| [Konsole](https://invent.kde.org/utilities/konsole)                          | [Kitty old protocol][kgp-old]          | ✅ Built-in                            |
| [foot](https://codeberg.org/dnkl/foot)                                       | [Sixel graphics format][sixel]         | ✅ Built-in                            |
| [Ghostty](https://github.com/ghostty-org/ghostty)                            | [Kitty unicode placeholders][kgp]      | ✅ Built-in                            |
| [Windows Terminal](https://github.com/microsoft/terminal) (>= v1.22.10352.0) | [Sixel graphics format][sixel]         | ✅ Built-in                            |
| [st with Sixel patch](https://github.com/bakkeby/st-flexipatch)              | [Sixel graphics format][sixel]         | ✅ Built-in                            |
| [Warp](https://www.warp.dev) (macOS/Linux only)                              | [Inline images protocol][iip]          | ✅ Built-in                            |
| [Tabby](https://github.com/Eugeny/tabby)                                     | [Inline images protocol][iip]          | ✅ Built-in                            |
| [VSCode](https://github.com/microsoft/vscode)                                | [Inline images protocol][iip]          | ✅ Built-in                            |
| [Rio](https://github.com/raphamorim/rio) (>= 0.3.9)                          | [Kitty unicode placeholders][kgp]      | ✅ Built-in                            |
| [Black Box](https://gitlab.gnome.org/raggesilver/blackbox)                   | [Sixel graphics format][sixel]         | ✅ Built-in                            |
| [Bobcat](https://github.com/ismail-yilmaz/Bobcat)                            | [Inline images protocol][iip]          | ✅ Built-in                            |
| X11 / Wayland                                                                | Window system protocol                 | ☑️ [Überzug++][ueberzug] required      |
| Fallback                                                                     | [ASCII art (Unicode block)][ascii-art] | ☑️ [Chafa][chafa] required (>= 1.16.0) |

See https://yazi-rs.github.io/docs/image-preview for details.

<!-- Protocols -->

[kgp]: https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders
[kgp-old]: https://github.com/sxyazi/yazi/blob/main/yazi-adapter/src/drivers/kgp_old.rs
[iip]: https://iterm2.com/documentation-images.html
[sixel]: https://www.vt100.net/docs/vt3xx-gp/chapter14.html
[ascii-art]: https://en.wikipedia.org/wiki/ASCII_art

<!-- Dependencies -->

[ueberzug]: https://github.com/jstkdng/ueberzugpp
[chafa]: https://hpjansson.org/chafa/

## Special Thanks

<img alt="RustRover logo" align="right" width="200" src="https://resources.jetbrains.com/storage/products/company/brand/logos/RustRover.svg">

Thanks to RustRover team for providing open-source licenses to support the maintenance of Yazi.

Active code contributors can contact @sxyazi to get a license (if any are still available).

## License

Yazi is MIT-licensed. For more information check the [LICENSE](LICENSE) file.
