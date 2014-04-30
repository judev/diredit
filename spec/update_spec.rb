require_relative "spec_helper"

describe UpdateCommand do

  it "is applied to matching inodes" do
    dirs = Directories.new([
      FileList.new('/tmp', [
        FileInfo.new(double(:ino => "abc123".scanf("%x").shift, :mode => "100644".scanf("%o").shift, :user => 'user', :group => 'group'), "/tmp/example.txt"),
        FileInfo.new(double(:ino => "abc124".scanf("%x").shift, :mode => "100644".scanf("%o").shift, :user => 'user', :group => 'group'), "/tmp/example2.txt"),
      ]),
    ])
    commands = Commands.parse([
      "abc123 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example.txt",
      "abc124 100644 user group 2014-04-30T10:11:12+01:00 /tmp/example2.txt",
    ])
    allow(commands["abc123"]).to receive(:apply_to) { |file, verbose| }
    allow(commands["abc124"]).to receive(:apply_to) { |file, verbose| }
    expect(commands["abc123"]).to receive(:apply_to)
    expect(commands["abc123"]).not_to receive(:apply_to)

    dirs.apply(commands)
  end

end
