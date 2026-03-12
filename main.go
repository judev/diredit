package main

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"os/user"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"
)

// --- Terminal detection ---

func isTerminal(fd uintptr) bool {
	var termios syscall.Termios
	_, _, err := syscall.Syscall6(syscall.SYS_IOCTL, fd, syscall.TCGETS, uintptr(unsafe.Pointer(&termios)), 0, 0, 0)
	return err == 0
}

// --- Username / group name lookups ---

func usernameFromUID(uid uint32) string {
	u, err := user.LookupId(strconv.FormatUint(uint64(uid), 10))
	if err != nil {
		return strconv.FormatUint(uint64(uid), 10)
	}
	return u.Username
}

func groupnameFromGID(gid uint32) string {
	g, err := user.LookupGroupId(strconv.FormatUint(uint64(gid), 10))
	if err != nil {
		return strconv.FormatUint(uint64(gid), 10)
	}
	return g.Name
}

func uidFromUsername(name string) (uint32, bool) {
	u, err := user.Lookup(name)
	if err != nil {
		return 0, false
	}
	uid, err := strconv.ParseUint(u.Uid, 10, 32)
	if err != nil {
		return 0, false
	}
	return uint32(uid), true
}

func gidFromGroupname(name string) (uint32, bool) {
	g, err := user.LookupGroup(name)
	if err != nil {
		return 0, false
	}
	gid, err := strconv.ParseUint(g.Gid, 10, 32)
	if err != nil {
		return 0, false
	}
	return uint32(gid), true
}

// --- Time formatting/parsing (ISO 8601 with timezone offset) ---

func formatMtime(t time.Time) string {
	return t.Format("2006-01-02T15:04:05-07:00")
}

func parseDatetime(s string) (time.Time, bool) {
	t, err := time.Parse("2006-01-02T15:04:05-07:00", s)
	if err != nil {
		return time.Time{}, false
	}
	return t, true
}

func formatTouchTime(t time.Time) string {
	return t.Local().Format("200601021504.05")
}

// --- Core data structures ---

type FileInfo struct {
	InodeHex string
	Mode     uint32
	UID      uint32
	GID      uint32
	Mtime    time.Time
	Filename string
}

func (f *FileInfo) ModeOctal() string {
	return fmt.Sprintf("%6o", f.Mode)
}

func (f *FileInfo) Username() string {
	return usernameFromUID(f.UID)
}

func (f *FileInfo) Groupname() string {
	return groupnameFromGID(f.GID)
}

func (f *FileInfo) MtimeString() string {
	return formatMtime(f.Mtime)
}

func (f *FileInfo) String() string {
	return fmt.Sprintf("%s %s %s %s %s %s",
		f.InodeHex,
		f.ModeOctal(),
		f.Username(),
		f.Groupname(),
		f.MtimeString(),
		f.Filename,
	)
}

type FileList struct {
	Name  string
	Files []FileInfo
}

func (fl *FileList) Find(inode string) *FileInfo {
	for i := range fl.Files {
		if fl.Files[i].InodeHex == inode {
			return &fl.Files[i]
		}
	}
	return nil
}

func listFiles(path string, recursive bool) (*FileList, error) {
	var files []FileInfo
	err := collectFiles(path, recursive, &files)
	if err != nil {
		return nil, err
	}
	return &FileList{Name: path, Files: files}, nil
}

func collectFiles(dir string, recursive bool, files *[]FileInfo) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	for _, entry := range entries {
		fullPath := filepath.Join(dir, entry.Name())

		info, err := entry.Info()
		if err != nil {
			return err
		}
		stat := info.Sys().(*syscall.Stat_t)

		filename := fullPath
		if strings.HasPrefix(filename, "./") {
			filename = filename[2:]
		}

		*files = append(*files, FileInfo{
			InodeHex: fmt.Sprintf("%x", stat.Ino),
			Mode:     uint32(stat.Mode),
			UID:      stat.Uid,
			GID:      stat.Gid,
			Mtime:    info.ModTime(),
			Filename: filename,
		})

		if recursive && info.IsDir() {
			if err := collectFiles(fullPath, true, files); err != nil {
				return err
			}
		}
	}
	return nil
}

func (fl *FileList) String() string {
	var b strings.Builder
	for i, f := range fl.Files {
		if i > 0 {
			b.WriteByte('\n')
		}
		b.WriteString(f.String())
	}
	return b.String()
}

type Directories struct {
	Dirs      []FileList
	ShowPaths bool
}

func listDirectories(paths []string, recursive, showPaths bool) Directories {
	var dirs []FileList
	for _, p := range paths {
		fl, err := listFiles(p, recursive)
		if err != nil {
			fmt.Fprintf(os.Stderr, "diredit: %s: %v\n", p, err)
			continue
		}
		dirs = append(dirs, *fl)
	}
	return Directories{Dirs: dirs, ShowPaths: showPaths}
}

func (d *Directories) Apply(commands map[string]Command, verbose bool) {
	for key, cmd := range commands {
		for i := range d.Dirs {
			if file := d.Dirs[i].Find(key); file != nil {
				cmd.ApplyTo(file, verbose)
				delete(commands, key)
				break
			}
		}
	}
}

func (d *Directories) String() string {
	var b strings.Builder
	for i, dir := range d.Dirs {
		if i > 0 {
			b.WriteByte('\n')
		}
		if d.ShowPaths {
			fmt.Fprintf(&b, "# Path: %s\n", dir.Name)
		}
		b.WriteString(dir.String())
	}
	return b.String()
}

// --- Commands ---

type Command struct {
	IsDelete bool
	Mode     uint32
	User     string
	Group    string
	Datetime string
	Mtime    time.Time
	Filename string
}

func (cmd *Command) ApplyTo(file *FileInfo, verbose bool) {
	if cmd.IsDelete {
		if err := os.Remove(file.Filename); err != nil {
			fmt.Fprintf(os.Stderr, "diredit: rm %s: %v\n", file.Filename, err)
		} else if verbose {
			fmt.Printf("rm -f %s\n", file.Filename)
		}
		return
	}

	if cmd.Mode != file.Mode {
		if err := os.Chmod(file.Filename, os.FileMode(cmd.Mode)); err != nil {
			fmt.Fprintf(os.Stderr, "diredit: chmod %s: %v\n", file.Filename, err)
		} else if verbose {
			fmt.Printf("chmod %o %s\n", cmd.Mode, file.Filename)
		}
	}

	if cmd.Datetime != file.MtimeString() {
		tv := syscall.NsecToTimeval(cmd.Mtime.UnixNano())
		atime := syscall.Timeval{Sec: 0, Usec: 0} // preserve atime
		times := [2]syscall.Timeval{atime, tv}
		if err := syscall.Utimes(file.Filename, times[:]); err != nil {
			fmt.Fprintf(os.Stderr, "diredit: touch %s: %v\n", file.Filename, err)
		} else if verbose {
			fmt.Printf("touch -m -t %s %s\n", formatTouchTime(cmd.Mtime), file.Filename)
		}
	}

	if cmd.User != file.Username() {
		uid, ok := uidFromUsername(cmd.User)
		if !ok {
			fmt.Fprintf(os.Stderr, "diredit: unknown user: %s\n", cmd.User)
		} else {
			// -1 means don't change
			if err := syscall.Chown(file.Filename, int(uid), -1); err != nil {
				fmt.Fprintf(os.Stderr, "diredit: chown %s: %v\n", file.Filename, err)
			} else if verbose {
				fmt.Printf("chown %s %s\n", cmd.User, file.Filename)
			}
		}
	}

	if cmd.Group != file.Groupname() {
		gid, ok := gidFromGroupname(cmd.Group)
		if !ok {
			fmt.Fprintf(os.Stderr, "diredit: unknown group: %s\n", cmd.Group)
		} else {
			if err := syscall.Chown(file.Filename, -1, int(gid)); err != nil {
				fmt.Fprintf(os.Stderr, "diredit: chgrp %s: %v\n", file.Filename, err)
			} else if verbose {
				fmt.Printf("chgrp %s %s\n", cmd.Group, file.Filename)
			}
		}
	}

	if cmd.Filename != file.Filename {
		if err := os.Rename(file.Filename, cmd.Filename); err != nil {
			fmt.Fprintf(os.Stderr, "diredit: mv %s %s: %v\n", file.Filename, cmd.Filename, err)
		} else if verbose {
			fmt.Printf("mv %s %s\n", file.Filename, cmd.Filename)
		}
	}
}

// --- Parsing ---

func parseCommands(lines []string) map[string]Command {
	commands := make(map[string]Command)
	for _, line := range lines {
		inode, cmd, ok := parseLine(line)
		if ok {
			commands[inode] = cmd
		}
	}
	return commands
}

func parseLine(line string) (string, Command, bool) {
	trimmed := strings.TrimSpace(line)

	if trimmed == "" || strings.HasPrefix(trimmed, "#") {
		return "", Command{}, false
	}

	// Extract inode (leading hex digits)
	inoEnd := 0
	for inoEnd < len(trimmed) && isHexDigit(trimmed[inoEnd]) {
		inoEnd++
	}
	if inoEnd == 0 {
		return "", Command{}, false
	}
	inode := trimmed[:inoEnd]
	rest := strings.TrimSpace(trimmed[inoEnd:])

	if rest == "" {
		return inode, Command{IsDelete: true}, true
	}

	modeStr, rest, ok := splitFirstWord(rest)
	if !ok {
		return "", Command{}, false
	}
	userName, rest, ok := splitFirstWord(rest)
	if !ok {
		return "", Command{}, false
	}
	groupName, rest, ok := splitFirstWord(rest)
	if !ok {
		return "", Command{}, false
	}

	if len(rest) < 25 {
		return "", Command{}, false
	}
	datetime := rest[:25]
	filename := strings.TrimSpace(rest[25:])

	if filename == "" {
		return "", Command{}, false
	}

	// Validate mode is all octal digits
	for _, c := range modeStr {
		if c < '0' || c > '9' {
			return "", Command{}, false
		}
	}

	mode, err := strconv.ParseUint(modeStr, 8, 32)
	if err != nil {
		return "", Command{}, false
	}

	mtime, ok := parseDatetime(datetime)
	if !ok {
		return "", Command{}, false
	}

	return inode, Command{
		Mode:     uint32(mode),
		User:     userName,
		Group:    groupName,
		Datetime: datetime,
		Mtime:    mtime,
		Filename: filename,
	}, true
}

func splitFirstWord(s string) (string, string, bool) {
	s = strings.TrimLeft(s, " \t")
	idx := strings.IndexAny(s, " \t")
	if idx <= 0 {
		if len(s) == 0 {
			return "", "", false
		}
		return s, "", true
	}
	return s[:idx], strings.TrimLeft(s[idx:], " \t"), true
}

func isHexDigit(b byte) bool {
	return (b >= '0' && b <= '9') || (b >= 'a' && b <= 'f') || (b >= 'A' && b <= 'F')
}

// --- CLI ---

type Options struct {
	Interactive bool
	HasStdin    bool
	Recursive   bool
	Verbose     bool
	Paths       []string
}

func printUsage() {
	fmt.Fprintln(os.Stderr, "Usage: diredit [options] [path]")
	fmt.Fprintln(os.Stderr, "    -h, --help                       Show this message")
	fmt.Fprintln(os.Stderr, "    -i, --interactive                 Launch $EDITOR to interactively edit directory listing")
	fmt.Fprintln(os.Stderr, "    -p, --non-interactive             Print listing instead of editing interactively")
	fmt.Fprintln(os.Stderr, "    -r, --recursive                   List recursively")
	fmt.Fprintln(os.Stderr, "    -v, --verbose                     Print every change as it is applied")
}

func parseArgs() Options {
	stdinIsTTY := isTerminal(os.Stdin.Fd())
	stdoutIsTTY := isTerminal(os.Stdout.Fd())
	hasStdin := !stdinIsTTY

	opts := Options{
		Interactive: stdinIsTTY && stdoutIsTTY,
		HasStdin:    hasStdin,
	}

	for _, arg := range os.Args[1:] {
		switch arg {
		case "-h", "--help":
			printUsage()
			os.Exit(0)
		case "-i", "--interactive":
			opts.Interactive = true
		case "-p", "--non-interactive":
			opts.Interactive = false
		case "-r", "--recursive":
			opts.Recursive = true
		case "-v", "--verbose":
			opts.Verbose = true
		default:
			if strings.HasPrefix(arg, "-") {
				fmt.Fprintf(os.Stderr, "diredit: invalid option: %s\n", arg)
				printUsage()
				os.Exit(1)
			}
			opts.Paths = append(opts.Paths, arg)
		}
	}

	if len(opts.Paths) == 0 {
		opts.Paths = []string{"."}
		if hasStdin {
			opts.Recursive = true
		}
	}

	return opts
}

const helpText = `## Empty lines and lines beginning with '#' are ignored.
## Do not edit the first column.
## Editing any of the other columns will cause those changes to be made once you save and quit.
## To delete a file remove everything on the line except the first column.
`

func run() error {
	opts := parseArgs()
	showPaths := opts.Verbose || opts.Interactive
	dirs := listDirectories(opts.Paths, opts.Recursive, showPaths)

	if opts.Interactive {
		tmpPath := filepath.Join(os.TempDir(), fmt.Sprintf("diredit-%d.diredit", os.Getpid()))

		content := dirs.String() + "\n" + helpText
		if err := os.WriteFile(tmpPath, []byte(content), 0644); err != nil {
			return fmt.Errorf("failed to write temp file: %w", err)
		}

		editor := os.Getenv("EDITOR")
		if editor == "" {
			editor = "/usr/bin/vi"
		}
		cmd := exec.Command(editor, tmpPath)
		cmd.Stdin = os.Stdin
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr

		if err := cmd.Run(); err != nil {
			os.Remove(tmpPath)
			return fmt.Errorf("failed to launch editor '%s': %w", editor, err)
		}

		edited, err := os.ReadFile(tmpPath)
		if err != nil {
			os.Remove(tmpPath)
			return fmt.Errorf("failed to read temp file: %w", err)
		}
		os.Remove(tmpPath)

		lines := strings.Split(string(edited), "\n")
		commands := parseCommands(lines)
		dirs.Apply(commands, opts.Verbose)
	} else if opts.HasStdin {
		scanner := bufio.NewScanner(os.Stdin)
		var lines []string
		for scanner.Scan() {
			lines = append(lines, scanner.Text())
		}
		commands := parseCommands(lines)
		dirs.Apply(commands, opts.Verbose)
	} else {
		fmt.Println(dirs.String())
	}

	return nil
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "diredit: %v\n", err)
		os.Exit(1)
	}
}
