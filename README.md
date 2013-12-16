# diredit

Edit directory contents (rename or delete files, edit permissions) using your text editor or in a unix pipeline.

Yes, you need to be careful.

```
Usage: diredit [options] [path]
    -h, --help                       Show this message
    -i, --interactive                Launch $EDITOR to interactively edit directory listing (default unless stdin and stdout are redirected)
    -p, --non-interactive            Print listing instead of editing interactively (default when stdin or stdout are directed to a file or pipe)
    -r, --recursive                  List recursively
    -v, --verbose                    Print every change as it is applied
```

## examples

rename .txt to .md
```
diredit ./tmp/ | sed 's/\.txt$/\.md/' | diredit
```

make files group-writable
```
diredit ./tmp/ | sed 's/100644/100664/' | diredit
```
