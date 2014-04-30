require_relative "spec_helper"

describe Commands do

  it "ignores comments" do
    parsed = Commands.parse(["# abc123"])
    expect(parsed).to eq({})
  end

  it "ignores blank lines" do
    parsed = Commands.parse([""])
    expect(parsed).to eq({})
  end

  it "creates delete commands when there is only an inode number" do
    parsed = Commands.parse(["abc123"])
    expect(parsed["abc123"]).to be_a(DeleteCommand)
  end

  it "creates update commands for well formed input" do
    parsed = Commands.parse(["abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt"])
    expect(parsed["abc123"]).to be_an(UpdateCommand)
  end

  it "creates update commands with correct content" do
    parsed = Commands.parse(["abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt"])
    update = parsed["abc123"]
    expect(update.info.mode_string).to eq("100644")
    expect(update.info.user).to eq("user")
    expect(update.info.group).to eq("group")
    expect(update.info.filename).to eq("/tmp/example.txt")
  end

  it "creates update commands ignoring extra whitespace" do
    parsed = Commands.parse(["  abc123  100644  user  group  2014-04-30T10:11:12+01:00   /tmp/example.txt "])
    update = parsed["abc123"]
    expect(update.info.mode_string).to eq("100644")
    expect(update.info.user).to eq("user")
    expect(update.info.group).to eq("group")
    expect(update.info.filename).to eq("/tmp/example.txt")
  end

end