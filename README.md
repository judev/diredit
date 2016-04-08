# diredit

Edit directory contents (rename or delete files, edit permissions) using your text editor or in a unix pipeline.

Running ```diredit``` directly will launch your ```$EDITOR``` and apply any changes after a save and quit. If you redirect or pipe standard output then it'll just print the directory contents. If you redirect or pipe something into it, it'll apply whichever changes it can.

Yes, you need to be careful.

```
Usage: diredit [options] [path] [path...]
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
diredit ./tmp/ | sed 's/ 100644/ 100664/' | diredit
```

## vim integration

Running ```diredit``` directly will launch your ```$EDITOR``` anyway, but the [vim-diredit](https://github.com/judev/vim-diredit) makes it a little nicer by hiding the inode column.
