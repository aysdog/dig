# dig

A fast command-line search tool for developers: content search by default (like ripgrep), fuzzy filename search, and a process monitor, all in one small Rust binary.

## Features

- Content search by default, with line numbers, colored output, and regex support
- Respects `.gitignore` automatically, the same way ripgrep does
- Long generated/minified lines get truncated around the match instead of flooding your terminal
- Fuzzy filename search that tolerates typos, ranked by match quality
- One flag (`-c`) controls case sensitivity everywhere, no auto-detection to second-guess
- Searches your current directory by default, or the whole computer with one flag
- Open a search result directly in your editor at the right line
- Process monitor grouped by category, refreshes itself automatically
- Live keyboard-driven interface with color and a clear selection highlight, no numbered menus
- Configurable default editor and file manager (`dig -config`)
- Parallel search across all CPU cores

## Install

Requires Rust (install via [rustup](https://rustup.rs) if you don't have it):

```
git clone https://github.com/YOUR_USERNAME/dig.git
cd dig
cargo build --release
```

The compiled binary will be at `target/release/dig`. Put it somewhere on your `PATH`:

```
sudo cp target/release/dig /usr/local/bin/dig
```

Or use the one-command installer (builds from source, requires Rust):

```
curl -fsSL https://aysdog.com/install-dig.sh | sh
```

## Usage

### Content search (the default)

```
dig TODO
```

Searches file contents in the current directory for `TODO`, case-insensitive, with line numbers and colored output. Automatically skips anything your `.gitignore` excludes, and anything that looks like a binary file.

```
dig TODO ~/work
```

Searches a specific directory instead of the current one.

```
dig -a TODO
```

Searches the whole computer instead of the current directory.

```
dig -c TODO
```

Case-sensitive: matches `TODO` only, not `todo` or `Todo`. Without `-c`, matching is case-insensitive.

```
dig -e '\d{3,}'
```

Treats the pattern as a regular expression instead of a literal string. Combine with `-c` for case-sensitive regex.

### Filename search

```
dig -f resume
```

Fuzzy filename search in the current directory. Typos are tolerated: `dig -f anrban` still finds `Anirban_Resume.pdf`.

```
dig -f -c resume
```

Exact, case-sensitive filename search instead of fuzzy.

```
dig -f -a resume
```

Fuzzy filename search across the whole computer.

### Process monitor

```
dig -p
```

Shows running processes grouped by category (System, Development, Browser, Media, Other), sorted by memory within each group. The list refreshes itself every couple of seconds, so a killed process disappears on its own and new ones show up without restarting.

### Configuration

```
dig -config
```

Sets your default code editor and file manager, used by the `o` (open in editor) and `f` (reveal in file manager) interactive keys. Falls back to `$EDITOR` and a few common editors/file managers if not set. Saved to `~/.config/dig/config` and can be changed anytime by running `dig -config` again.

## Interactive controls

Content search:

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection down / up |
| `o` | Open the match in your editor, at the matching line |
| `c` | Copy the file's path to clipboard |
| `n` | Show the next batch of results |
| `q` | Quit |

File search:

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection down / up |
| `f` | Reveal selected file in file manager |
| `c` | Copy selected file's path to clipboard |
| `n` | Show the next batch of results |
| `q` | Quit |

Process monitor:

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection down / up |
| `x` | Kill selected process (asks for confirmation) |
| `q` | Quit |

Killing is blocked for PID 1, the current process and its parent, and a list of essential system process names (`systemd`, `sshd`, display servers, and similar), regardless of what you select.

## Platform notes

The live keyboard interface uses raw terminal mode, currently implemented for Unix (Linux and macOS). On other platforms, `dig` falls back to a numbered menu so it still works, just without live key control.

## How it's built

- `main.rs` - CLI entry point, interactive UI loops, colored output
- `content_search.rs` - parallel content search: memory-mapped files, literal or regex matching, long-line truncation
- `find.rs` - parallel filename search
- `matcher.rs` - typo-tolerant subsequence matching and scoring
- `process.rs` - process listing, categorization, auto-refresh, safe termination
- `walker.rs` - shared directory-walking rules, including `.gitignore` awareness
- `keyboard.rs` - raw terminal key handling
- `color.rs` - terminal color output, auto-disabled when piped to another program
- `config.rs` - persistent user settings (`dig -config`)

## License

MIT. See [LICENSE](LICENSE).
