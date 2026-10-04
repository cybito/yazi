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

## `custom` 分支：输入法模式上报

Yazi 可选地向 `ime-control` 或 Herdr 报告命令态与文本态、焦点和暂停/恢复/退出；帮助搜索、主输入框及插件 alternate input 的 Insert/Replace 模式均属于文本态。无 Herdr marker 时，本地图形终端使用持久 Unix socket（`~/.local/state/infra-as-code/ime-control/run/control.sock`），启用期间命令键等待服务切换／回读完成的 `scope:applied` ACK。`HERDR_IME_INTENT=1` 与 `HERDR_ENV=1` 先于 GUI/SSH 判定选择 `pane.input_intent.stream`，必须有可信 `HERDR_SOCKET_PATH` 和互斥的 pane/Popup identity；`scope:recorded` 仅表示 server 保存了意图，聚焦 client 自行等待本机 applied ACK。非法／矛盾 marker 禁用 reporter，不回退至本地主机 daemon 或 server 输入源。外部编辑器或 shell 接管前同步暂停；reporter 暂停失败时关闭连接，普通终端让渡仍继续。child 返回或 SIGCONT 不自动恢复前台意图，下一个真实 key/FocusIn 才按当前编辑模式恢复。

完整请求（写入至读完 ACK）共享 4 秒期限，分片不会续期。对端发完最终 ACK 后立即关闭是合法生命周期；Yazi 会先消费完整 ACK，再使已关闭连接的缓存授权失效。

输入源的采样、英文切换、用户手选、恢复与监督全部由本机服务负责，Yazi 不运行 macism/Fcitx 命令、不保存原源，也不自行轮询；text 只释放暂态英文，不强制中文。transport 选择、平台、marker、socket、peer、ACK、焦点或后端失败（包括 `FOCUS_UNVERIFIED` / `BACKEND_UNAVAILABLE`）会静默关闭本进程 reporter、清空缓存意图，直到下次启动才重新尝试；当前按键、搜索文本、Esc/q、暂停/恢复、外部 child 与退出均继续普通 CLI 行为，不弹通知或输出缺失组件错误。这种退化不表示 applied、recorded 或英文保护；正常 `inactive` 仍保持后台保护边界，不永久禁用 reporter。无 marker 的 SSH／无本地图形会话不建立 lease。Herdr server 与 client 均须显式启用 `[experimental].ime_control`，该源码候选不等于已部署或真实 GUI 验收。

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
