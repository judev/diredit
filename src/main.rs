use std::collections::HashMap;
use std::env;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs;
use std::io::{self, BufRead, IsTerminal};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process;
use std::time::{Duration, SystemTime};

// --- Username / group name lookups via libc ---

fn username_from_uid(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return uid.to_string();
        }
        CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
    }
}

fn groupname_from_gid(gid: u32) -> String {
    unsafe {
        let gr = libc::getgrgid(gid);
        if gr.is_null() {
            return gid.to_string();
        }
        CStr::from_ptr((*gr).gr_name).to_string_lossy().into_owned()
    }
}

fn uid_from_username(name: &str) -> Option<u32> {
    let c_name = CString::new(name).ok()?;
    unsafe {
        let pw = libc::getpwnam(c_name.as_ptr());
        if pw.is_null() { None } else { Some((*pw).pw_uid) }
    }
}

fn gid_from_groupname(name: &str) -> Option<u32> {
    let c_name = CString::new(name).ok()?;
    unsafe {
        let gr = libc::getgrnam(c_name.as_ptr());
        if gr.is_null() { None } else { Some((*gr).gr_gid) }
    }
}

// --- Time formatting/parsing (ISO 8601 with timezone offset) ---

fn format_mtime(mtime: SystemTime) -> String {
    let secs = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };

    let offset_secs = tm.tm_gmtoff;
    let offset_sign = if offset_secs >= 0 { '+' } else { '-' };
    let offset_abs = offset_secs.unsigned_abs() as u64;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        offset_sign,
        offset_abs / 3600,
        (offset_abs % 3600) / 60,
    )
}

fn parse_datetime(s: &str) -> Option<SystemTime> {
    if s.len() < 25 {
        return None;
    }

    let year: i32 = s[0..4].parse().ok()?;
    let month: i32 = s[5..7].parse().ok()?;
    let day: i32 = s[8..10].parse().ok()?;
    let hour: i32 = s[11..13].parse().ok()?;
    let min: i32 = s[14..16].parse().ok()?;
    let sec: i32 = s[17..19].parse().ok()?;
    let tz_sign = &s[19..20];
    let tz_hours: i64 = s[20..22].parse().ok()?;
    let tz_mins: i64 = s[23..25].parse().ok()?;

    let tz_offset_secs = match tz_sign {
        "+" => tz_hours * 3600 + tz_mins * 60,
        "-" => -(tz_hours * 3600 + tz_mins * 60),
        _ => return None,
    };

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = -1;

    let epoch = unsafe { libc::timegm(&mut tm) };
    if epoch == -1 {
        return None;
    }
    let utc_secs = epoch - tz_offset_secs;

    if utc_secs < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(utc_secs as u64))
}

fn format_touch_time(mtime: SystemTime) -> String {
    let secs = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };

    format!(
        "{:04}{:02}{:02}{:02}{:02}.{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    )
}

// --- Libc helper ---

fn libc_chown(c_path: &CStr, uid: u32, gid: u32) -> io::Result<()> {
    let ret = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn set_mtime(c_path: &CStr, mtime: SystemTime) -> io::Result<()> {
    let duration = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let times = [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: duration.as_secs() as libc::time_t, tv_nsec: 0 },
    ];
    let ret = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// --- Core data structures ---

struct FileInfo {
    inode_hex: String,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: SystemTime,
    filename: String,
}

impl FileInfo {
    fn mode_octal(&self) -> String {
        format!("{:6o}", self.mode)
    }

    fn username(&self) -> String {
        username_from_uid(self.uid)
    }

    fn groupname(&self) -> String {
        groupname_from_gid(self.gid)
    }

    fn mtime_string(&self) -> String {
        format_mtime(self.mtime)
    }
}

impl fmt::Display for FileInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {} {} {}",
            self.inode_hex,
            self.mode_octal(),
            self.username(),
            self.groupname(),
            self.mtime_string(),
            self.filename,
        )
    }
}

struct FileList {
    name: String,
    files: Vec<FileInfo>,
}

impl FileList {
    fn find(&self, inode: &str) -> Option<&FileInfo> {
        self.files.iter().find(|f| f.inode_hex == inode)
    }

    fn list(path: &str, recursive: bool) -> io::Result<Self> {
        let mut files = Vec::new();
        Self::collect_files(Path::new(path), recursive, &mut files)?;
        Ok(FileList { name: path.to_string(), files })
    }

    fn collect_files(dir: &Path, recursive: bool, files: &mut Vec<FileInfo>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let mut filename = entry.path().to_string_lossy().into_owned();

            if filename.starts_with("./") {
                filename = filename[2..].to_string();
            }

            files.push(FileInfo {
                inode_hex: format!("{:x}", metadata.ino()),
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                filename,
            });

            if recursive && metadata.is_dir() {
                Self::collect_files(&entry.path(), true, files)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for FileList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, file) in self.files.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{file}")?;
        }
        Ok(())
    }
}

struct Directories {
    dirs: Vec<FileList>,
    show_paths: bool,
}

impl Directories {
    fn apply(&self, commands: &mut HashMap<String, Command>, verbose: bool) {
        let keys: Vec<String> = commands.keys().cloned().collect();
        for key in keys {
            for dir in &self.dirs {
                if let Some(file) = dir.find(&key) {
                    if let Some(cmd) = commands.get(&key) {
                        cmd.apply_to(file, verbose);
                        commands.remove(&key);
                        break;
                    }
                }
            }
        }
    }

    fn list(paths: &[String], recursive: bool, show_paths: bool) -> Self {
        let dirs: Vec<FileList> = paths
            .iter()
            .filter_map(|p| match FileList::list(p, recursive) {
                Ok(fl) => Some(fl),
                Err(e) => {
                    eprintln!("diredit: {p}: {e}");
                    None
                }
            })
            .collect();
        Directories { dirs, show_paths }
    }
}

impl fmt::Display for Directories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, dir) in self.dirs.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            if self.show_paths {
                writeln!(f, "# Path: {}", dir.name)?;
            }
            write!(f, "{dir}")?;
        }
        Ok(())
    }
}

// --- Commands ---

enum Command {
    Delete,
    Update {
        mode: u32,
        user: String,
        group: String,
        datetime: String,
        mtime: SystemTime,
        filename: String,
    },
}

impl Command {
    fn apply_to(&self, file: &FileInfo, verbose: bool) {
        match self {
            Command::Delete => {
                if let Err(e) = fs::remove_file(&file.filename) {
                    eprintln!("diredit: rm {}: {e}", file.filename);
                } else if verbose {
                    println!("rm -f {}", file.filename);
                }
            }
            Command::Update { mode, user, group, datetime, mtime, filename, .. } => {
                let c_path = match CString::new(file.filename.as_bytes()) {
                    Ok(p) => p,
                    Err(_) => return,
                };

                if *mode != file.mode {
                    match fs::set_permissions(&file.filename, fs::Permissions::from_mode(*mode)) {
                        Err(e) => eprintln!("diredit: chmod {}: {e}", file.filename),
                        Ok(()) if verbose => println!("chmod {:o} {}", mode, file.filename),
                        _ => {}
                    }
                }

                if *datetime != file.mtime_string() {
                    match set_mtime(&c_path, *mtime) {
                        Err(e) => eprintln!("diredit: touch {}: {e}", file.filename),
                        Ok(()) if verbose => {
                            println!("touch -m -t {} {}", format_touch_time(*mtime), file.filename);
                        }
                        _ => {}
                    }
                }

                if *user != file.username() {
                    match uid_from_username(user) {
                        None => eprintln!("diredit: unknown user: {user}"),
                        Some(uid) => match libc_chown(&c_path, uid, u32::MAX) {
                            Err(e) => eprintln!("diredit: chown {}: {e}", file.filename),
                            Ok(()) if verbose => println!("chown {user} {}", file.filename),
                            _ => {}
                        },
                    }
                }

                if *group != file.groupname() {
                    match gid_from_groupname(group) {
                        None => eprintln!("diredit: unknown group: {group}"),
                        Some(gid) => match libc_chown(&c_path, u32::MAX, gid) {
                            Err(e) => eprintln!("diredit: chgrp {}: {e}", file.filename),
                            Ok(()) if verbose => println!("chgrp {group} {}", file.filename),
                            _ => {}
                        },
                    }
                }

                if *filename != file.filename {
                    match fs::rename(&file.filename, filename) {
                        Err(e) => eprintln!("diredit: mv {} {filename}: {e}", file.filename),
                        Ok(()) if verbose => println!("mv {} {filename}", file.filename),
                        _ => {}
                    }
                }
            }
        }
    }
}

// --- Parsing ---

fn parse_commands(lines: &[String]) -> HashMap<String, Command> {
    let mut commands = HashMap::new();
    for line in lines {
        if let Some((inode, cmd)) = parse_line(line) {
            commands.insert(inode, cmd);
        }
    }
    commands
}

/// Parse a single line, returning the inode key and command.
fn parse_line(line: &str) -> Option<(String, Command)> {
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let ino_end = trimmed
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(trimmed.len());
    if ino_end == 0 {
        return None;
    }
    let inode = trimmed[..ino_end].to_string();
    let rest = trimmed[ino_end..].trim();

    if rest.is_empty() {
        return Some((inode, Command::Delete));
    }

    let (mode_str, rest) = split_first_word(rest)?;
    let (user, rest) = split_first_word(rest)?;
    let (group, rest) = split_first_word(rest)?;

    if rest.len() < 25 {
        return None;
    }
    let datetime = &rest[..25];
    let filename = rest[25..].trim();

    if filename.is_empty() {
        return None;
    }

    if !mode_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let mode = u32::from_str_radix(mode_str, 8).ok()?;
    let mtime = parse_datetime(datetime)?;

    Some((
        inode,
        Command::Update {
            mode,
            user: user.to_string(),
            group: group.to_string(),
            datetime: datetime.to_string(),
            mtime,
            filename: filename.to_string(),
        },
    ))
}

fn split_first_word(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let end = s.find(|c: char| c.is_whitespace()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], s[end..].trim_start()))
}

// --- CLI ---

struct Options {
    interactive: bool,
    has_stdin: bool,
    recursive: bool,
    verbose: bool,
    paths: Vec<String>,
}

fn print_usage() {
    eprintln!("Usage: diredit [options] [path]");
    eprintln!("    -h, --help                       Show this message");
    eprintln!("    -i, --interactive                 Launch $EDITOR to interactively edit directory listing");
    eprintln!("    -p, --non-interactive             Print listing instead of editing interactively");
    eprintln!("    -r, --recursive                   List recursively");
    eprintln!("    -v, --verbose                     Print every change as it is applied");
}

fn parse_args() -> Options {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = io::stdout().is_terminal();
    let has_stdin = !stdin_is_tty;

    let mut opts = Options {
        interactive: stdin_is_tty && stdout_is_tty,
        has_stdin,
        recursive: false,
        verbose: false,
        paths: Vec::new(),
    };

    let args: Vec<String> = env::args().skip(1).collect();
    for arg in &args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                process::exit(0);
            }
            "-i" | "--interactive" => opts.interactive = true,
            "-p" | "--non-interactive" => opts.interactive = false,
            "-r" | "--recursive" => opts.recursive = true,
            "-v" | "--verbose" => opts.verbose = true,
            s if s.starts_with('-') => {
                eprintln!("diredit: invalid option: {s}");
                print_usage();
                process::exit(1);
            }
            path => opts.paths.push(path.to_string()),
        }
    }

    if opts.paths.is_empty() {
        opts.paths.push(".".to_string());
        if has_stdin {
            opts.recursive = true;
        }
    }

    opts
}

const HELP_TEXT: &str = "\
## Empty lines and lines beginning with '#' are ignored.
## Do not edit the first column.
## Editing any of the other columns will cause those changes to be made once you save and quit.
## To delete a file remove everything on the line except the first column.
";

fn run() -> Result<(), String> {
    let opts = parse_args();
    let show_paths = opts.verbose || opts.interactive;
    let dirs = Directories::list(&opts.paths, opts.recursive, show_paths);

    if opts.interactive {
        let tmp_path = env::temp_dir().join(format!("diredit-{}.diredit", process::id()));

        let content = format!("{dirs}\n{HELP_TEXT}");
        fs::write(&tmp_path, &content)
            .map_err(|e| format!("failed to write temp file: {e}"))?;

        let editor = env::var("EDITOR").unwrap_or_else(|_| "/usr/bin/vi".to_string());
        let status = process::Command::new(&editor)
            .arg(&tmp_path)
            .status()
            .map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                format!("failed to launch editor '{editor}': {e}")
            })?;

        if !status.success() {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("editor exited with status {status}"));
        }

        let edited = fs::read_to_string(&tmp_path).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            format!("failed to read temp file: {e}")
        })?;

        let _ = fs::remove_file(&tmp_path);

        let lines: Vec<String> = edited.lines().map(String::from).collect();
        let mut commands = parse_commands(&lines);
        dirs.apply(&mut commands, opts.verbose);
    } else if opts.has_stdin {
        let stdin = io::stdin();
        let lines: Vec<String> = stdin.lock().lines().filter_map(|l| l.ok()).collect();
        let mut commands = parse_commands(&lines);
        dirs.apply(&mut commands, opts.verbose);
    } else {
        println!("{dirs}");
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("diredit: {e}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comments() {
        let lines = vec!["# abc123".to_string()];
        let parsed = parse_commands(&lines);
        assert!(parsed.is_empty());
    }

    #[test]
    fn ignores_blank_lines() {
        let lines = vec!["".to_string()];
        let parsed = parse_commands(&lines);
        assert!(parsed.is_empty());
    }

    #[test]
    fn creates_delete_commands_for_inode_only() {
        let lines = vec!["abc123".to_string()];
        let parsed = parse_commands(&lines);
        assert!(parsed.contains_key("abc123"));
        assert!(matches!(parsed["abc123"], Command::Delete { .. }));
    }

    #[test]
    fn creates_update_commands_for_well_formed_input() {
        let lines = vec![
            "abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt".to_string(),
        ];
        let parsed = parse_commands(&lines);
        assert!(parsed.contains_key("abc123"));
        assert!(matches!(parsed["abc123"], Command::Update { .. }));
    }

    #[test]
    fn parses_update_command_fields() {
        let lines = vec![
            "abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt".to_string(),
        ];
        let parsed = parse_commands(&lines);
        match &parsed["abc123"] {
            Command::Update { mode, user, group, filename, .. } => {
                assert_eq!(*mode, 0o100644);
                assert_eq!(user, "user");
                assert_eq!(group, "group");
                assert_eq!(filename, "/tmp/example.txt");
            }
            _ => panic!("expected Update command"),
        }
    }

    #[test]
    fn handles_extra_whitespace() {
        let lines = vec![
            "  abc123  100644  user  group  2014-04-30T10:11:12+01:00   /tmp/example.txt "
                .to_string(),
        ];
        let parsed = parse_commands(&lines);
        assert!(parsed.contains_key("abc123"));
        match &parsed["abc123"] {
            Command::Update { user, group, filename, .. } => {
                assert_eq!(user, "user");
                assert_eq!(group, "group");
                assert_eq!(filename, "/tmp/example.txt");
            }
            _ => panic!("expected Update command"),
        }
    }

    #[test]
    fn parse_datetime_roundtrip() {
        let dt = "2014-04-30T10:11:12+01:00";
        assert!(parse_datetime(dt).is_some());
    }

    #[test]
    fn parse_datetime_negative_offset() {
        let dt = "2014-04-30T10:11:12-05:00";
        assert!(parse_datetime(dt).is_some());
    }

    #[test]
    fn file_list_produces_parseable_output() {
        let tmp = env::temp_dir().join("diredit-test-roundtrip");
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("testfile.txt");
        fs::write(&test_file, "hello").unwrap();

        let fl = FileList::list(tmp.to_str().unwrap(), false).unwrap();
        let output = fl.to_string();
        let lines: Vec<String> = output.lines().map(String::from).collect();
        let commands = parse_commands(&lines);

        assert!(!commands.is_empty());

        let _ = fs::remove_file(&test_file);
        let _ = fs::remove_dir(&tmp);
    }

    #[test]
    fn directories_to_string_with_verbose() {
        let tmp = env::temp_dir().join("diredit-test-verbose");
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("vtest.txt");
        fs::write(&test_file, "data").unwrap();

        let dirs = Directories::list(
            &[tmp.to_str().unwrap().to_string()],
            false,
            true, // show_paths
        );
        let output = dirs.to_string();
        assert!(output.contains("# Path:"));

        let _ = fs::remove_file(&test_file);
        let _ = fs::remove_dir(&tmp);
    }
}
