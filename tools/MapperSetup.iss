[Setup]
AppId={{EAA3912B-623E-4100-A39A-A126C72EB651}
AppName=Mapper for Rust
AppVersion={#ReleaseVersion}
VersionInfoVersion={#NumericVersion}
AppPublisher=Papyrus for Rust
AppPublisherURL=https://github.com/Papyrus-for-Rust/Mapper-Studio
DefaultDirName={localappdata}\Programs\Mapper for Rust
DefaultGroupName=Mapper for Rust
DisableStartupPrompt=yes
DisableWelcomePage=yes
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyMemo=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
WizardStyle=modern dynamic
SetupIconFile=installer/mapper.ico
WizardSmallImageFile=installer/mapper-mark.png
UninstallDisplayIcon={app}\mapper.ico
CloseApplications=yes
RestartApplications=no
OutputBaseFilename=Mapper-for-Rust-{#ReleaseVersion}-Setup-win-x64
Compression=lzma2/fast
LZMANumBlockThreads=1
SolidCompression=yes

[Messages]
WizardReady=Install Mapper for Rust
ReadyLabel1=Install Mapper for this Windows account. Setup adds a Start menu shortcut.
ReadyLabel2b=
FinishedHeadingLabel=Mapper is ready
FinishedLabel=Open Mapper now or find it in the Start menu.

[Files]
Source: "{#PackageDirectory}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

Source: "installer\mapper.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Mapper for Rust"; Filename: "{app}\mapper-studio.exe"; WorkingDir: "{app}"; IconFilename: "{app}\mapper.ico"

[Run]
Filename: "{app}\mapper-studio.exe"; Description: "Launch Mapper for Rust"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
