[Setup]
AppId={{EAA3912B-623E-4100-A39A-A126C72EB651}
AppName=Mapper for Rust
AppVersion={#ReleaseVersion}
VersionInfoVersion={#NumericVersion}
AppPublisher=Papyrus for Rust
AppPublisherURL=https://github.com/Papyrus-for-Rust/Mapper-Studio
DefaultDirName={localappdata}\Programs\Mapper for Rust
DefaultGroupName=Mapper for Rust
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern
UninstallDisplayIcon={app}\mapper-studio.exe
CloseApplications=yes
RestartApplications=no
LicenseFile={#PackageDirectory}\LICENSE
OutputBaseFilename=Mapper-for-Rust-{#ReleaseVersion}-Setup-win-x64
Compression=lzma2/fast
LZMANumBlockThreads=1
SolidCompression=yes

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#PackageDirectory}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Mapper for Rust"; Filename: "{app}\mapper-studio.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Mapper for Rust"; Filename: "{app}\mapper-studio.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\mapper-studio.exe"; Description: "Launch Mapper for Rust"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
