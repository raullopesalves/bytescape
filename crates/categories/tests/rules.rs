use std::fs;

use categories::{Cleanup, Rules, NONE};

/// Asserts each `(path, is_dir, expected category id)`; `""` means no category.
fn check(rules: &Rules, cases: &[(&str, bool, &str)]) {
    let mut failed = Vec::new();
    for &(path, is_dir, want) in cases {
        let got = rules.classify(path, is_dir);
        let got = rules.category(got).map_or("", |c| c.id.as_str());
        if got != want {
            failed.push(format!("{path}: expected {want:?}, got {got:?}"));
        }
    }
    assert!(failed.is_empty(), "\n{}", failed.join("\n"));
}

#[test]
#[rustfmt::skip]
fn default_rules() {
    check(
        &Rules::defaults(),
        &[
            // Extensions, ignoring case; longest extension first.
            ("C:/Users/r/Videos/clip.MP4", false, "videos"),
            ("C:/Users/r/Pictures/a.jpeg", false, "pictures"),
            ("C:/Users/r/Music/song.flac", false, "music"),
            ("C:/Users/r/Documents/cv.pdf", false, "documents"),
            ("D:/backup/site.tar.gz", false, "archives"),
            ("D:/backup/log.gz", false, "archives"),
            ("D:/vm/disk.vhdx", false, "disk-images"),
            ("C:/src/proj/main.rs", false, "code"),
            ("C:/Users/r/Downloads/setup.exe", false, "programs"),
            // No match: files fall back, folders get nothing.
            ("C:/Users/r/notes", false, "other"),
            ("C:/Users/r/.gitignore", false, "other"),
            ("C:/Users/r/archive.", false, "other"),
            ("C:/Users/r/Documents", true, ""),
            // Path rules beat extension rules.
            ("C:/Windows/System32/kernel32.dll", false, "windows"),
            ("C:/src/proj/node_modules/pkg/index.js", false, "dev-deps"),
            ("C:/Program Files/App/readme.txt", false, "programs"),
            // The deepest matching folder wins.
            ("C:/Windows", true, "windows"),
            ("C:/Windows/Temp", true, "caches"),
            ("C:/Windows/Temp/x.log", false, "caches"),
            ("c:/windows/temp/A.TMP", false, "caches"),
            ("C:/Program Files (x86)/Steam/steamapps/common/Game/game.exe", false, "games"),
            ("C:/Program Files (x86)/Steam/steamapps/common", true, "games"),
            ("C:/Program Files (x86)/Steam/steamapps", true, "programs"),
            // Folders named like caches only count under AppData.
            ("C:/Users/r/AppData/Local/Google/Chrome/User Data/Default/Cache/Cache_Data/f_01", false, "caches"),
            ("C:/Steam/steamapps/common/Game/Cache/data.bin", false, "games"),
            ("C:/Windows/System32/DriverStore/Cache/x.inf", false, "windows"),
            ("C:/Users/r/AppData/Roaming/Code/CachedExtensionVSIXs/ext-1.2", false, "caches"),
            ("C:/Users/r/AppData/Local/Mozilla/Firefox/Profiles/p/cache2/entries/AB", false, "caches"),
            ("C:/Users/r/AppData/Roaming/app/Settings/prefs.json", false, "other"),
            // An exception: Store apps' LocalCache, where extensions apply again.
            ("C:/Users/r/AppData/Local/Packages/App_1/LocalCache", true, ""),
            ("C:/Users/r/AppData/Local/Packages/App_1/LocalCache/Roaming/a.dat", false, "other"),
            ("C:/Users/r/AppData/Local/Packages/App_1/LocalCache/Roaming/v.mp4", false, "videos"),
            ("C:/Users/r/AppData/Local/Packages/App_1/LocalCache/Local/x/Cache/f", false, "caches"),
            ("C:/Users/r/AppData/Local/Packages/App_1/AC/INetCache/f", false, "caches"),
            ("C:/Users/r/.cache/pip/http/a.bin", false, "caches"),
            // AI models: by folder (deeper than ~/.cache) or by extension.
            ("C:/Users/r/.cache/huggingface/hub/model.bin", false, "ai-models"),
            ("C:/Users/r/.ollama/models/blobs/sha256-0123", false, "ai-models"),
            ("D:/models/llama-3-8b.Q4_K_M.gguf", false, "ai-models"),
            ("D:/models/sdxl.SAFETENSORS", false, "ai-models"),
            ("C:/Users/r/Documents/My Games/Game/Saved/save1.sav", false, "games"),
            ("C:/Users/r/AppData/Roaming/.minecraft/saves/w/level.dat", false, "games"),
            ("C:/src/proj/.git/objects/pack/pack-1.pack", false, "code"),
            // File rules are anchored; folder rules do not match files.
            ("C:/pagefile.sys", false, "windows"),
            ("C:/Users/r/pagefile.sys", false, "other"),
            ("C:/Users/r/node_modules", false, "other"),
            ("D:/$Recycle.Bin/S-1-5-21/$RABC.mp4", false, "recycle-bin"),
            // Backslashes and the \\?\ prefix.
            (r"C:\Windows\notepad.exe", false, "windows"),
            (r"\\?\C:\Windows\Temp\a.txt", false, "caches"),
            (r"\\?\UNC\server\share\proj\node_modules\a.js", false, "dev-deps"),
        ],
    );
}

#[test]
fn default_rules_metadata() {
    let rules = Rules::defaults();
    let other = rules.id_of("other").unwrap();
    assert_eq!(rules.fallback(), other);
    assert_eq!(rules.category(NONE), None);
    let caches = rules.category(rules.id_of("caches").unwrap()).unwrap();
    assert_eq!(caches.name, "Caches and temp");
    assert_eq!(caches.cleanup, Cleanup::Safe);
    assert_eq!(rules.categories().len(), 15);
}

#[test]
#[rustfmt::skip]
fn user_rules_add_and_override() {
    let user = r#"
        [[category]]
        id = "games"
        paths = ['D:\MyGames\**']

        [[category]]
        id = "code"
        extensions = [".MD"]

        [[category]]
        id = "archives"
        cleanup = "safe"

        [[category]]
        id = "work"
        name = "Work"
        color = "red"
        cleanup = "never"
        paths = ["?:/Users/*/Documents/Work/**"]

        [[category]]
        id = "mine"
        paths = ["?:/Windows/**"]
    "#;
    let rules = Rules::with_user_rules("user.toml", user).unwrap();
    check(
        &rules,
        &[
            // Added to an existing category; old rules still apply.
            ("D:/MyGames/Game/data.pak", false, "games"),
            ("C:/x/steamapps/common/G/a.exe", false, "games"),
            // A user extension rule replaces the default one.
            ("C:/notes/readme.md", false, "code"),
            ("C:/Users/r/Documents/Work/plan.pdf", false, "work"),
            ("C:/Users/r/Documents/plan.pdf", false, "documents"),
            // Same folder matched by default and user rules: user wins.
            ("C:/Windows/notepad.exe", false, "mine"),
            // Deeper default rules still beat it.
            ("C:/Windows/Temp/a.txt", false, "caches"),
        ],
    );
    let games = rules.category(rules.id_of("games").unwrap()).unwrap();
    assert_eq!(games.name, "Games");
    let archives = rules.category(rules.id_of("archives").unwrap()).unwrap();
    assert_eq!(archives.cleanup, Cleanup::Safe);
    assert_eq!(archives.name, "Archives");
    let mine = rules.category(rules.id_of("mine").unwrap()).unwrap();
    assert_eq!(
        (mine.name.as_str(), mine.cleanup),
        ("mine", Cleanup::Review)
    );
}

#[test]
fn bad_rules_are_errors() {
    let cases = [
        ("not toml [", "TOML parse error"),
        ("[[category]]\nid = \"a\"\nsize = 3", "unknown field"),
        (
            "[[category]]\nid = \"a\"\ncleanup = \"maybe\"",
            "unknown variant",
        ),
        ("fallback = \"nope\"", "fallback \"nope\" is not a category"),
        ("[[category]]\nid = \"\"", "empty id"),
        (
            "[[category]]\nid = \"a\"\nextensions = [\".\"]",
            "empty extension",
        ),
        (
            "[[category]]\nid = \"a\"\npaths = [\"**/[ab\"]",
            "category \"a\": ",
        ),
        (
            "[[category]]\nid = \"a\"\npaths = [\"!**/[ab\"]",
            "unclosed",
        ),
    ];
    for (text, want) in cases {
        let err = Rules::with_user_rules("user.toml", text).unwrap_err();
        assert!(
            err.to_string().contains(want),
            "{text:?}: expected {want:?} in {err}"
        );
    }
}

#[test]
fn apply_to_scanned_tree() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    fs::create_dir_all(r.join("proj/node_modules/pkg")).unwrap();
    fs::create_dir(r.join("empty")).unwrap();
    fs::write(r.join("clip.mp4"), vec![0u8; 1000]).unwrap();
    fs::write(r.join("notes"), b"hello").unwrap();
    fs::write(r.join("proj/main.rs"), b"fn main() {}").unwrap();
    fs::write(r.join("proj/node_modules/pkg/index.js"), vec![0u8; 300]).unwrap();
    fs::write(r.join("proj/node_modules/pkg/README.md"), vec![0u8; 20]).unwrap();

    let mut tree = scanner::scan(r, &scanner::ScanOptions::default()).unwrap();
    let rules = Rules::defaults();
    rules.apply(&mut tree);

    let cat = |rel: &str| {
        let id = (0..tree.len() as u32)
            .find(|&i| tree.path(i) == r.join(rel))
            .unwrap_or_else(|| panic!("{rel} not in tree"));
        rules
            .category(tree.node(id).category)
            .map_or("", |c| c.id.as_str())
    };
    assert_eq!(cat("clip.mp4"), "videos");
    assert_eq!(cat("notes"), "other");
    assert_eq!(cat("proj/main.rs"), "code");
    assert_eq!(cat("proj"), "");
    assert_eq!(cat("empty"), "");
    assert_eq!(cat("proj/node_modules"), "dev-deps");
    assert_eq!(cat("proj/node_modules/pkg/index.js"), "dev-deps");
    assert_eq!(cat("proj/node_modules/pkg/README.md"), "dev-deps");

    let totals = rules.totals(&tree);
    let t = |id: &str| totals[rules.id_of(id).unwrap() as usize];
    assert_eq!((t("videos").files, t("videos").size), (1, 1000));
    assert_eq!((t("dev-deps").files, t("dev-deps").size), (2, 320));
    assert_eq!(totals[NONE as usize].files, 0);
    let root = tree.node(tree.root());
    assert_eq!(totals.iter().map(|t| t.files).sum::<u64>(), root.files);
    assert_eq!(totals.iter().map(|t| t.size).sum::<u64>(), root.size);
}
