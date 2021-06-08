# pso2-modpatcher

Tool for patching PSO2 data directories with new loose files

## Usage

    pso2-modpatcher.exe patchdir datadir

Structure your patch directory like so:

    win32
    -- icefilename_ice
    -- -- 1
    -- -- -- file1.text
    -- -- 2
    -- -- -- file2.text
    -- -- -- anewfile.fun

- The `_ice` suffix is necessary to identify ICEs to patch.
- Directories without the suffix will be treated as real directories.
- Loose files outside of `_ice` directories will be ignored.
- There must be at least a `1` or `2` directory in an `_ice` directory. The
  absence of both is treated as an error.
- Files not present in the original ICE will be added at the _end_ of the
  corresponding group.
- Patch directories may not be named "backup".
- A backup of each patched ICE will be stored in `datadir/BACKUP` with the same
  directory tree. e.g. `win32/abcd` will be copied to `backup/win32/abcd`.
  **This backup _will be overwritten on subsequent runs,_** so be careful.

## License

MIT or Apache 2.0
