use std::collections::HashMap;
use std::env;
use std::ffi::CStr;
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
    let c_name = std::ffi::CString::new(name).ok()?;
    unsafe {
        let pw = libc::getpwnam(c_name.as_ptr());
        if pw.is_null() {
            None
        } else {
            Some((*pw).pw_uid)
        }
    }
}

fn gid_from_groupname(name: &str) -> Option<u32> {
    let c_name = std::ffi::CString::new(name).ok()?;
    unsafe {
        let gr = libc::getgrnam(c_name.as_ptr());
        if gr.is_null() {
            None
        } else {
            Some((*gr).gr_gid)
        }
    }
}

// --- Time formatting/parsing (ISO 8601 with timezone offset) ---

/// Format a SystemTime as ISO 8601 local time with UTC offset: 2014-04-30T10:11:12+01:00
fn format_mtime(mtime: SystemTime) -> String {
    let duration = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration.as_secs() as i64;

    // Use libc to get local time with timezone
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs as *const i64, &mut tm);
    }

    let offset_secs = tm.tm_gmtoff;
    let offset_sign = if offset_secs >= 0 { '+' } else { '-' };
    let offset_abs = offset_secs.unsigned_abs() as u64;
    let offset_hours = offset_abs / 3600;
    let offset_mins = (offset_abs % 3600) / 60;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        offset_sign,
        offset_hours,
        offset_mins,
    )
}

/// Parse an ISO 8601 datetime string like "2014-04-30T10:11:12+01:00" into a SystemTime.
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

    // Convert to UTC timestamp
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = -1;

    // timegm treats the fields as UTC, then we subtract the offset
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

/// Format SystemTime for touch-style verbose output
fn format_touch_time(mtime: SystemTime) -> String {
    let duration = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = duration.as_secs() as i64;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&secs as *const i64, &mut tm);
    }
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

// --- Core data structures ---

struct FileInfo {
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: SystemTime,
    filename: String,
}

impl FileInfo {
    fn inode_hex(&self) -> String {
        format!("{:x}", self.inode)
    }

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

    fn format_line(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.inode_hex(),
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
    files: HashMap<String, FileInfo>,
}

impl FileList {
    fn new(name: String, files: Vec<FileInfo>) -> Self {
        let mut map = HashMap::new();
        for f in files {
            map.insert(f.inode_hex(), f);
        }
        FileList { name, files: map }
    }

    fn to_string_repr(&self) -> String {
        self.files
            .values()
            .map(|f| f.format_line())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn apply(&self, command: &dyn Command, verbose: bool) -> bool {
        if let Some(file) = self.files.get(&command.inode()) {
            command.apply_to(file, verbose);
            return true;
        }
        false
    }

    fn list(path: &str, recursive: bool) -> io::Result<Self> {
        let mut files = Vec::new();
        Self::collect_files(Path::new(path), recursive, &mut files)?;
        Ok(FileList::new(path.to_string(), files))
    }

    fn collect_files(
        dir: &Path,
        recursive: bool,
        files: &mut Vec<FileInfo>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let mut filename = entry.path().to_string_lossy().into_owned();

            // Strip leading "./" like the Ruby version
            if filename.starts_with("./") {
                filename = filename[2..].to_string();
            }

            files.push(FileInfo {
                inode: metadata.ino(),
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

struct Directories {
    dirs: Vec<FileList>,
    verbose: bool,
    interactive: bool,
}

impl Directories {
    fn new(dirs: Vec<FileList>, verbose: bool, interactive: bool) -> Self {
        Directories {
            dirs,
            verbose,
            interactive,
        }
    }

    fn to_string_repr(&self) -> String {
        let mut parts = Vec::new();
        for dir in &self.dirs {
            if self.verbose || self.interactive {
                parts.push(format!("# Path: {}", dir.name));
            }
            parts.push(dir.to_string_repr());
        }
        parts.join("\n")
    }

    fn apply(&self, commands: &mut HashMap<String, Box<dyn Command>>, verbose: bool) {
        let keys: Vec<String> = commands.keys().cloned().collect();
        for key in keys {
            for dir in &self.dirs {
                if let Some(cmd) = commands.get(&key) {
                    if dir.apply(cmd.as_ref(), verbose) {
                        commands.remove(&key);
                        break;
                    }
                }
            }
        }
    }

    fn list(paths: &[String], recursive: bool, verbose: bool, interactive: bool) -> Self {
        let dirs: Vec<FileList> = paths
            .iter()
            .filter_map(|p| match FileList::list(p, recursive) {
                Ok(fl) => Some(fl),
                Err(e) => {
                    eprintln!("diredit: {}: {}", p, e);
                    None
                }
            })
            .collect();
        Self::new(dirs, verbose, interactive)
    }
}

// --- Commands ---

trait Command {
    fn inode(&self) -> String;
    fn apply_to(&self, file: &FileInfo, verbose: bool);
}

struct DeleteCommand {
    ino: String,
}

impl Command for DeleteCommand {
    fn inode(&self) -> String {
        self.ino.clone()
    }

    fn apply_to(&self, file: &FileInfo, verbose: bool) {
        if let Err(e) = fs::remove_file(&file.filename) {
            eprintln!("diredit: rm {}: {}", file.filename, e);
        } else if verbose {
            println!("rm -f {}", file.filename);
        }
    }
}

struct UpdateCommand {
    ino: String,
    mode_string: String,
    user: String,
    group: String,
    datetime: String,
    filename: String,
}

impl UpdateCommand {
    fn parsed_mode(&self) -> Option<u32> {
        u32::from_str_radix(self.mode_string.trim(), 8).ok()
    }

    fn parsed_mtime(&self) -> Option<SystemTime> {
        parse_datetime(&self.datetime)
    }
}

impl Command for UpdateCommand {
    fn inode(&self) -> String {
        self.ino.clone()
    }

    fn apply_to(&self, file: &FileInfo, verbose: bool) {
        // chmod
        if self.mode_string.trim() != file.mode_octal().trim() {
            if let Some(mode) = self.parsed_mode() {
                if let Err(e) =
                    fs::set_permissions(&file.filename, fs::Permissions::from_mode(mode))
                {
                    eprintln!("diredit: chmod {}: {}", file.filename, e);
                } else if verbose {
                    println!("chmod {} {}", self.mode_string.trim(), file.filename);
                }
            }
        }

        // touch (mtime)
        if self.datetime != file.mtime_string() {
            if let Some(new_mtime) = self.parsed_mtime() {
                let c_path = match std::ffi::CString::new(file.filename.as_bytes()) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let duration = new_mtime
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO);
                let times = [
                    libc::timespec {
                        tv_sec: 0,
                        tv_nsec: libc::UTIME_OMIT,
                    }, // atime: keep unchanged
                    libc::timespec {
                        tv_sec: duration.as_secs() as libc::time_t,
                        tv_nsec: 0,
                    }, // mtime
                ];
                let ret = unsafe {
                    libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0)
                };
                if ret != 0 {
                    eprintln!(
                        "diredit: touch {}: {}",
                        file.filename,
                        io::Error::last_os_error()
                    );
                } else if verbose {
                    println!(
                        "touch -m -t {} {}",
                        format_touch_time(new_mtime),
                        file.filename
                    );
                }
            }
        }

        // chown
        if self.user != file.username() {
            if let Some(new_uid) = uid_from_username(&self.user) {
                let c_path = match std::ffi::CString::new(file.filename.as_bytes()) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let ret = unsafe { libc::chown(c_path.as_ptr(), new_uid, u32::MAX) };
                if ret != 0 {
                    eprintln!(
                        "diredit: chown {}: {}",
                        file.filename,
                        io::Error::last_os_error()
                    );
                } else if verbose {
                    println!("chown {} {}", self.user, file.filename);
                }
            } else {
                eprintln!("diredit: unknown user: {}", self.user);
            }
        }

        // chgrp
        if self.group != file.groupname() {
            if let Some(new_gid) = gid_from_groupname(&self.group) {
                let c_path = match std::ffi::CString::new(file.filename.as_bytes()) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let ret = unsafe { libc::chown(c_path.as_ptr(), u32::MAX, new_gid) };
                if ret != 0 {
                    eprintln!(
                        "diredit: chgrp {}: {}",
                        file.filename,
                        io::Error::last_os_error()
                    );
                } else if verbose {
                    println!("chgrp {} {}", self.group, file.filename);
                }
            } else {
                eprintln!("diredit: unknown group: {}", self.group);
            }
        }

        // rename
        if self.filename != file.filename {
            if let Err(e) = fs::rename(&file.filename, &self.filename) {
                eprintln!("diredit: mv {} {}: {}", file.filename, self.filename, e);
            } else if verbose {
                println!("mv {} {}", file.filename, self.filename);
            }
        }
    }
}

// --- Parsing ---

fn parse_commands(lines: &[String]) -> HashMap<String, Box<dyn Command>> {
    let mut commands: HashMap<String, Box<dyn Command>> = HashMap::new();

    for line in lines {
        if let Some(parsed) = parse_line(line) {
            match parsed {
                ParsedLine::Delete(ino) => {
                    commands.insert(ino.clone(), Box::new(DeleteCommand { ino }));
                }
                ParsedLine::Update {
                    ino,
                    mode,
                    user,
                    group,
                    datetime,
                    filename,
                } => {
                    commands.insert(
                        ino.clone(),
                        Box::new(UpdateCommand {
                            ino,
                            mode_string: mode,
                            user,
                            group,
                            datetime,
                            filename,
                        }),
                    );
                }
            }
        }
    }

    commands
}

enum ParsedLine {
    Delete(String),
    Update {
        ino: String,
        mode: String,
        user: String,
        group: String,
        datetime: String,
        filename: String,
    },
}

/// Parse a single line matching the Ruby regex pattern.
fn parse_line(line: &str) -> Option<ParsedLine> {
    let trimmed = line.trim();

    // Skip comments and blank lines
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    // Extract inode: leading hex chars
    let ino_end = trimmed
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(trimmed.len());
    if ino_end == 0 {
        return None;
    }
    let ino = &trimmed[..ino_end];

    let rest = trimmed[ino_end..].trim();

    if rest.is_empty() {
        // Delete command: only inode
        return Some(ParsedLine::Delete(ino.to_string()));
    }

    // Parse: mode user group datetime filename
    let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
    let mode = parts.next()?.trim();
    let rest = parts.next()?.trim();

    let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
    let user = parts.next()?.trim();
    let rest = parts.next()?.trim();

    let mut parts = rest.splitn(2, |c: char| c.is_whitespace());
    let group = parts.next()?.trim();
    let rest = parts.next()?.trim();

    // datetime is exactly 25 chars: YYYY-MM-DDTHH:MM:SS+HH:MM
    if rest.len() < 25 {
        return None;
    }
    let datetime = &rest[..25];
    let filename = rest[25..].trim();

    if filename.is_empty() {
        return None;
    }

    // Validate mode is numeric
    if !mode.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some(ParsedLine::Update {
        ino: ino.to_string(),
        mode: mode.to_string(),
        user: user.to_string(),
        group: group.to_string(),
        datetime: datetime.to_string(),
        filename: filename.to_string(),
    })
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
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage();
                process::exit(0);
            }
            "-i" | "--interactive" => opts.interactive = true,
            "-p" | "--non-interactive" => opts.interactive = false,
            "-r" | "--recursive" => opts.recursive = true,
            "-v" | "--verbose" => opts.verbose = true,
            arg if arg.starts_with('-') => {
                eprintln!("diredit: invalid option: {}", arg);
                print_usage();
                process::exit(1);
            }
            path => opts.paths.push(path.to_string()),
        }
        i += 1;
    }

    if opts.paths.is_empty() {
        opts.paths.push(".".to_string());
        if has_stdin {
            opts.recursive = true; // simplifies use in pipeline
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

fn main() {
    let opts = parse_args();
    let dirs = Directories::list(&opts.paths, opts.recursive, opts.verbose, opts.interactive);

    if opts.interactive {
        // Write to temp file, launch editor, read back
        let tmp_dir = env::temp_dir();
        let tmp_path = tmp_dir.join(format!("diredit-{}.diredit", process::id()));

        let content = format!("{}\n{}", dirs.to_string_repr(), HELP_TEXT);
        if let Err(e) = fs::write(&tmp_path, &content) {
            eprintln!("diredit: failed to write temp file: {}", e);
            process::exit(1);
        }

        let editor = env::var("EDITOR").unwrap_or_else(|_| "/usr/bin/vi".to_string());
        let status = process::Command::new(&editor).arg(&tmp_path).status();

        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                let _ = fs::remove_file(&tmp_path);
                eprintln!("diredit: editor exited with status {}", s);
                process::exit(1);
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                eprintln!("diredit: failed to launch editor '{}': {}", editor, e);
                process::exit(1);
            }
        }

        let edited = match fs::read_to_string(&tmp_path) {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                eprintln!("diredit: failed to read temp file: {}", e);
                process::exit(1);
            }
        };

        let _ = fs::remove_file(&tmp_path);

        let lines: Vec<String> = edited.lines().map(String::from).collect();
        let mut commands = parse_commands(&lines);
        dirs.apply(&mut commands, opts.verbose);
    } else if opts.has_stdin {
        // Pipeline input mode
        let stdin = io::stdin();
        let lines: Vec<String> = stdin.lock().lines().filter_map(|l| l.ok()).collect();
        let mut commands = parse_commands(&lines);
        dirs.apply(&mut commands, opts.verbose);
    } else {
        // Print mode
        println!("{}", dirs.to_string_repr());
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
        assert_eq!(parsed["abc123"].inode(), "abc123");
    }

    #[test]
    fn creates_update_commands_for_well_formed_input() {
        let lines = vec![
            "abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt".to_string(),
        ];
        let parsed = parse_commands(&lines);
        assert!(parsed.contains_key("abc123"));
    }

    #[test]
    fn handles_extra_whitespace() {
        let lines = vec![
            "  abc123  100644  user  group  2014-04-30T10:11:12+01:00   /tmp/example.txt "
                .to_string(),
        ];
        let parsed = parse_commands(&lines);
        assert!(parsed.contains_key("abc123"));
    }

    #[test]
    fn parse_datetime_roundtrip() {
        let dt = "2014-04-30T10:11:12+01:00";
        let parsed = parse_datetime(dt);
        assert!(parsed.is_some());
    }

    #[test]
    fn parse_datetime_negative_offset() {
        let dt = "2014-04-30T10:11:12-05:00";
        let parsed = parse_datetime(dt);
        assert!(parsed.is_some());
    }

    #[test]
    fn file_list_produces_parseable_output() {
        // Create a temp directory, list it, then parse the output back
        let tmp = std::env::temp_dir().join("diredit-test-roundtrip");
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("testfile.txt");
        fs::write(&test_file, "hello").unwrap();

        let fl = FileList::list(tmp.to_str().unwrap(), false).unwrap();
        let output = fl.to_string_repr();
        let lines: Vec<String> = output.lines().map(String::from).collect();
        let commands = parse_commands(&lines);

        // Should have parsed at least our test file
        assert!(!commands.is_empty());

        // Cleanup
        let _ = fs::remove_file(&test_file);
        let _ = fs::remove_dir(&tmp);
    }

    #[test]
    fn directories_to_string_with_verbose() {
        let tmp = std::env::temp_dir().join("diredit-test-verbose");
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("vtest.txt");
        fs::write(&test_file, "data").unwrap();

        let dirs = Directories::list(
            &[tmp.to_str().unwrap().to_string()],
            false,
            true,  // verbose
            false, // not interactive
        );
        let output = dirs.to_string_repr();
        assert!(output.contains("# Path:"));

        let _ = fs::remove_file(&test_file);
        let _ = fs::remove_dir(&tmp);
    }
}
