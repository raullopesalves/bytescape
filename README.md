# diskmap (working name)

A disk usage analyzer for Windows. This is step 1 of the plan: the scanner
library and a command-line tool. The desktop app comes in step 3.

## What's here

| Folder | What it is |
| --- | --- |
| `crates/scanner` | The scanner library: parallel folder walk, node tree, folder totals, hardlinks, size on disk, OneDrive placeholders |
| `crates/cli` | `diskmap-cli`, which scans a folder and prints where the space went |

## Setting up Windows (one time)

1. Install Rust: `winget install Rustlang.Rustup`, or use the installer from rustup.rs.
2. Install the Visual Studio Build Tools with the **Desktop development with C++** workload.
   The Rust installer offers to do this for you.
3. Open a new terminal and check that `cargo --version` works.

## Trying it

From this folder:

```
cargo test
cargo run --release -p diskmap-cli -- C:\Users\<you>
```

Options go after the `--`, before the path:

```
cargo run --release -p diskmap-cli -- --no-hardlinks C:\Windows
```

Useful options:

```
--top 25                 list 25 folders and files instead of 15
--json scan.json         also write the tree to a JSON file
--no-hardlinks           count hardlinked files every time they appear
--slow-listing           Windows: use the older, slower folder listing
--show-errors            list everything that could not be read
```

Scanning `C:\` works without admin rights. Some system folders will be listed
as unreadable, which is expected.

## Things to check on Windows

This version lists each folder with one Windows call that also returns every
file's internal ID, so hardlinks are found without opening files. It has not
been run on Windows yet. Please send back:

1. The output of `cargo test`. There should be two extra Windows-only tests
   (`fast_listing_matches_std_listing` and `junctions_are_listed_as_links`).
2. `cargo run --release -p diskmap-cli -- C:\Windows`, run twice; send the second.
   It should report about the same size as before (27.6 GB) but much faster.
3. `cargo run --release -p diskmap-cli -- --slow-listing C:\Windows`, the old method,
   to confirm both give the same totals.

## License

MIT or Apache-2.0, at your option.
