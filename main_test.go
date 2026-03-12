package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestIgnoresComments(t *testing.T) {
	lines := []string{"# abc123"}
	parsed := parseCommands(lines)
	if len(parsed) != 0 {
		t.Fatal("expected empty commands for comment line")
	}
}

func TestIgnoresBlankLines(t *testing.T) {
	lines := []string{""}
	parsed := parseCommands(lines)
	if len(parsed) != 0 {
		t.Fatal("expected empty commands for blank line")
	}
}

func TestCreatesDeleteCommandsForInodeOnly(t *testing.T) {
	lines := []string{"abc123"}
	parsed := parseCommands(lines)
	cmd, ok := parsed["abc123"]
	if !ok {
		t.Fatal("expected command for abc123")
	}
	if !cmd.IsDelete {
		t.Fatal("expected Delete command")
	}
}

func TestCreatesUpdateCommandsForWellFormedInput(t *testing.T) {
	lines := []string{"abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt"}
	parsed := parseCommands(lines)
	cmd, ok := parsed["abc123"]
	if !ok {
		t.Fatal("expected command for abc123")
	}
	if cmd.IsDelete {
		t.Fatal("expected Update command, got Delete")
	}
}

func TestParsesUpdateCommandFields(t *testing.T) {
	lines := []string{"abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt"}
	parsed := parseCommands(lines)
	cmd := parsed["abc123"]

	if cmd.Mode != 0o100644 {
		t.Fatalf("expected mode 0o100644, got %o", cmd.Mode)
	}
	if cmd.User != "user" {
		t.Fatalf("expected user 'user', got '%s'", cmd.User)
	}
	if cmd.Group != "group" {
		t.Fatalf("expected group 'group', got '%s'", cmd.Group)
	}
	if cmd.Filename != "/tmp/example.txt" {
		t.Fatalf("expected filename '/tmp/example.txt', got '%s'", cmd.Filename)
	}
}

func TestHandlesExtraWhitespace(t *testing.T) {
	lines := []string{"  abc123  100644  user  group  2014-04-30T10:11:12+01:00   /tmp/example.txt "}
	parsed := parseCommands(lines)
	cmd, ok := parsed["abc123"]
	if !ok {
		t.Fatal("expected command for abc123")
	}

	if cmd.User != "user" {
		t.Fatalf("expected user 'user', got '%s'", cmd.User)
	}
	if cmd.Group != "group" {
		t.Fatalf("expected group 'group', got '%s'", cmd.Group)
	}
	if cmd.Filename != "/tmp/example.txt" {
		t.Fatalf("expected filename '/tmp/example.txt', got '%s'", cmd.Filename)
	}
}

func TestParseDatetimeRoundtrip(t *testing.T) {
	dt := "2014-04-30T10:11:12+01:00"
	_, ok := parseDatetime(dt)
	if !ok {
		t.Fatal("expected successful parse")
	}
}

func TestParseDatetimeNegativeOffset(t *testing.T) {
	dt := "2014-04-30T10:11:12-05:00"
	_, ok := parseDatetime(dt)
	if !ok {
		t.Fatal("expected successful parse")
	}
}

func TestFileListProducesParseableOutput(t *testing.T) {
	tmp := filepath.Join(os.TempDir(), "diredit-test-roundtrip-go")
	os.MkdirAll(tmp, 0755)
	testFile := filepath.Join(tmp, "testfile.txt")
	os.WriteFile(testFile, []byte("hello"), 0644)
	defer func() {
		os.Remove(testFile)
		os.Remove(tmp)
	}()

	fl, err := listFiles(tmp, false)
	if err != nil {
		t.Fatalf("listFiles failed: %v", err)
	}

	output := fl.String()
	lines := splitLines(output)
	commands := parseCommands(lines)

	if len(commands) == 0 {
		t.Fatal("expected non-empty commands from file list output")
	}
}

func TestDirectoriesToStringWithVerbose(t *testing.T) {
	tmp := filepath.Join(os.TempDir(), "diredit-test-verbose-go")
	os.MkdirAll(tmp, 0755)
	testFile := filepath.Join(tmp, "vtest.txt")
	os.WriteFile(testFile, []byte("data"), 0644)
	defer func() {
		os.Remove(testFile)
		os.Remove(tmp)
	}()

	dirs := listDirectories([]string{tmp}, false, true)
	output := dirs.String()

	if !contains(output, "# Path:") {
		t.Fatal("expected output to contain '# Path:'")
	}
}

func splitLines(s string) []string {
	var lines []string
	for _, line := range splitByNewline(s) {
		lines = append(lines, line)
	}
	return lines
}

func splitByNewline(s string) []string {
	result := []string{}
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			result = append(result, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		result = append(result, s[start:])
	}
	return result
}

func contains(s, substr string) bool {
	return len(s) >= len(substr) && searchString(s, substr)
}

func searchString(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
