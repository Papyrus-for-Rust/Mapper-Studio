# Mapper CLI

Run `mapper.exe --help` to see the available commands. Add `--help` to a command for its options.

```powershell
./mapper.exe project new island.mapper --seed 12345 --size 4000
./mapper.exe project validate island.mapper
./mapper.exe generate island.mapper --output previews/island
```

The preview can run with Mapper's independent terrain engine. To generate a server map, use a compatible Rust installation and a source-bound recipe. The desktop can prepare that installation and save the recipe for you.

```powershell
$rustRoot = 'C:/Steam/steamapps/common/Rust'
./mapper.exe installation inspect $rustRoot
./mapper.exe installation prepare $rustRoot
./mapper.exe generate server-island.mapper --rust-root $rustRoot --map island.map
```

Use a fresh output path. Keep the exported map with its recipe if you plan to reproduce it after changing installations.

## Decode a support code

In Mapper, open **Help → Support snapshot** to review what the code contains. Copy the code and share it with the person helping you.

```powershell
./mapper.exe support decode "MPS1-PASTE_CODE_HERE"
```

The command prints JSON locally. Anyone with the code can decode it.
