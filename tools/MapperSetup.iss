#define Branding "..\src\mapper-studio\assets\branding"

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
SetupIconFile={#Branding}\papyrus.ico
WizardSmallImageFile={#Branding}\papyrus.png
WizardImageFile={#Branding}\papyrus-wizard.png
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
Source: "{#PackageDirectory}\mapper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDirectory}\update-channel.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDirectory}\*"; DestDir: "{app}"; Excludes: "\mapper.exe,\update-channel.json"; Flags: ignoreversion recursesubdirs createallsubdirs

Source: "{#Branding}\papyrus.ico"; DestDir: "{app}"; DestName: "mapper.ico"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Mapper for Rust"; Filename: "{app}\mapper-studio.exe"; WorkingDir: "{app}"; IconFilename: "{app}\mapper.ico"

[Run]
Filename: "{app}\mapper-studio.exe"; Description: "Launch Mapper for Rust"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[Code]
const
  MapperUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{EAA3912B-623E-4100-A39A-A126C72EB651}_is1';

var
  MaintenancePage: TInputOptionWizardPage;
  InstalledVersion, InstallerVersion, UninstallerPath: String;
  InstallAction, UninstallAction: Integer;
  InstallActionLabel: String;
  SetupIsOlder, ExitingAfterMaintenance, CheckedForNewerSetup: Boolean;

function TakeIdentifier(var Version: String; const Separator: String): String;
var
  Boundary: Integer;
begin
  Boundary := Pos(Separator, Version);
  if Boundary = 0 then
  begin
    Result := Version;
    Version := '';
  end
  else
  begin
    Result := Copy(Version, 1, Boundary - 1);
    Delete(Version, 1, Boundary);
  end;
end;

function IsNumericIdentifier(const Identifier: String): Boolean;
var
  Index: Integer;
begin
  Result := False;
  if Identifier = '' then
    Exit;
  for Index := 1 to Length(Identifier) do
    if (Identifier[Index] < '0') or (Identifier[Index] > '9') then
      Exit;
  Result := True;
end;

function CompareIdentifiers(Left, Right: String): Integer;
var
  LeftPart, RightPart: String;
  LeftNumeric, RightNumeric: Boolean;
begin
  Result := 0;
  while (Left <> '') and (Right <> '') do
  begin
    LeftPart := TakeIdentifier(Left, '.');
    RightPart := TakeIdentifier(Right, '.');
    LeftNumeric := IsNumericIdentifier(LeftPart);
    RightNumeric := IsNumericIdentifier(RightPart);
    if LeftNumeric and RightNumeric and (Length(LeftPart) <> Length(RightPart)) then
      Result := Length(LeftPart) - Length(RightPart)
    else if LeftNumeric <> RightNumeric then
    begin
      if LeftNumeric then
        Result := -1
      else
        Result := 1;
    end
    else
      Result := CompareStr(LeftPart, RightPart);
    if Result <> 0 then
      Exit;
  end;
  if Left <> '' then
    Result := 1
  else if Right <> '' then
    Result := -1;
end;

function CompareReleaseVersions(Left, Right: String): Integer;
var
  LeftCore, RightCore: String;
begin
  { Mapper releases use SemVer without build metadata. }
  LeftCore := TakeIdentifier(Left, '-');
  RightCore := TakeIdentifier(Right, '-');
  Result := CompareIdentifiers(LeftCore, RightCore);
  if Result <> 0 then
    Exit;
  if (Left = '') and (Right <> '') then
    Result := 1
  else if (Left <> '') and (Right = '') then
    Result := -1
  else
    Result := CompareIdentifiers(Left, Right);
end;

function FindUninstaller: String;
var
  RegisteredCommand, InstallDir, Candidate, Name, Suffix: String;
begin
  Result := '';
  if not RegQueryStringValue(HKCU, MapperUninstallKey, 'InstallLocation', InstallDir) or
     not RegQueryStringValue(HKCU, MapperUninstallKey, 'UninstallString', RegisteredCommand) then
    Exit;

  { Inno registers a quoted executable path. Run the file directly. }
  Candidate := RemoveQuotes(RegisteredCommand);
  Name := Lowercase(ExtractFileName(Candidate));
  Suffix := Copy(Name, 6, Length(Name) - 9);
  if (CompareText(ExtractFileDir(Candidate), RemoveBackslashUnlessRoot(InstallDir)) = 0) and
     (Copy(Name, 1, 5) = 'unins') and (ExtractFileExt(Name) = '.exe') and
     IsNumericIdentifier(Suffix) and FileExists(Candidate) then
    Result := Candidate;
end;

procedure InitializeWizard;
var
  Description, Choices: String;
  VersionOrder: Integer;
begin
  InstallerVersion := '{#ReleaseVersion}';
  if not RegQueryStringValue(HKCU, MapperUninstallKey, 'DisplayVersion', InstalledVersion) or
     (InstalledVersion = '') then
    Exit;

  UninstallerPath := FindUninstaller;
  VersionOrder := CompareReleaseVersions(InstallerVersion, InstalledVersion);
  SetupIsOlder := VersionOrder < 0;
  InstallAction := -1;
  UninstallAction := -1;
  Description := 'Installed: ' + InstalledVersion + '    Setup: ' + InstallerVersion;
  Choices := 'Choose an action. Your projects and settings are kept.';
  if SetupIsOlder then
    Choices := 'Use a newer installer to update or reinstall Mapper.'
  else if UninstallerPath = '' then
    Choices := 'Reinstall or update Mapper to restore its Windows uninstaller.';
  MaintenancePage := CreateInputOptionPage(wpSelectTasks, 'Manage Mapper for Rust',
    Description, Choices, True, False);

  if not SetupIsOlder then
  begin
    if VersionOrder > 0 then
      InstallActionLabel := 'Update'
    else
      InstallActionLabel := 'Reinstall';
    InstallAction := MaintenancePage.Add(InstallActionLabel + ' Mapper');
  end;
  if UninstallerPath <> '' then
    UninstallAction := MaintenancePage.Add('Uninstall Mapper');
  MaintenancePage.SelectedValueIndex := InstallAction;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
  if MaintenancePage <> nil then
    Result := ((PageID = MaintenancePage.ID) and WizardSilent) or
      ((PageID = wpReady) and not WizardSilent);
end;

procedure ShowMaintenanceAction(Sender: TObject);
begin
  if (InstallAction >= 0) and (MaintenancePage.SelectedValueIndex = InstallAction) then
    WizardForm.NextButton.Caption := InstallActionLabel
  else
    WizardForm.NextButton.Caption := 'Uninstall';
  WizardForm.NextButton.Enabled := MaintenancePage.SelectedValueIndex >= 0;
end;

procedure OfferNewerSetup;
var
  TempDir, LatestVersion, Url, Digest, FileName, InstallerPath: String;
  Output: TExecOutput;
  DownloadPage: TDownloadWizardPage;
  ExitCode: Integer;
begin
  if CheckedForNewerSetup or WizardSilent then
    Exit;
  CheckedForNewerSetup := True;
  if ExpandConstant('{param:SKIPSETUPUPDATE|0}') = '1' then
    Exit;

  TempDir := ExpandConstant('{tmp}');
  try
    ExtractTemporaryFile('mapper.exe');
    ExtractTemporaryFile('update-channel.json');
    if not ExecAndCaptureOutput(TempDir + '\mapper.exe',
        'setup-update-check ' + AddQuotes(InstalledVersion), TempDir,
        SW_HIDE, ewWaitUntilTerminated, ExitCode, Output) then
      Exit;
    if (ExitCode <> 0) or Output.Error or (GetArrayLength(Output.StdOut) <> 3) then
      Exit;
    LatestVersion := Output.StdOut[0];
    Url := Output.StdOut[1];
    Digest := Output.StdOut[2];
  except
    Log('Mapper setup update check skipped: ' + GetExceptionMessage);
    Exit;
  end;

  if MsgBox('Mapper ' + LatestVersion + ' is available. Download and install it instead?',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) <> IDYES then
    Exit;

  FileName := 'mapper-newer-setup.exe';
  InstallerPath := TempDir + '\' + FileName;
  DownloadPage := CreateDownloadPage('Downloading Mapper ' + LatestVersion,
    'The newer installer will open after its download is verified.', nil);
  DownloadPage.Add(Url, FileName, Digest);
  DownloadPage.Show;
  try
    try
      DownloadPage.Download;
    except
      if not DownloadPage.AbortedByUser then
        MsgBox('The newer installer could not be downloaded or verified. You can continue with this installer.',
          mbError, MB_OK);
      Exit;
    end;
  finally
    DownloadPage.Hide;
  end;

  WizardForm.Hide;
  if not Exec(InstallerPath, '/SKIPSETUPUPDATE=1', TempDir,
      SW_SHOWNORMAL, ewWaitUntilTerminated, ExitCode) then
  begin
    WizardForm.Show;
    MsgBox('Could not open the newer installer. ' + SysErrorMessage(ExitCode), mbError, MB_OK);
    Exit;
  end;
  if ExitCode = 0 then
  begin
    ExitingAfterMaintenance := True;
    WizardForm.Close;
  end
  else
    WizardForm.Show;
end;

procedure CurPageChanged(PageID: Integer);
begin
  if (MaintenancePage <> nil) and (PageID = MaintenancePage.ID) then
  begin
    MaintenancePage.CheckListBox.OnClickCheck := @ShowMaintenanceAction;
    ShowMaintenanceAction(nil);
    OfferNewerSetup;
  end
  else if (MaintenancePage = nil) and (PageID = wpReady) then
    OfferNewerSetup;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if ExitingAfterMaintenance then
    Result := 'The newer installer has already finished.'
  else if SetupIsOlder then
    Result := 'Mapper ' + InstalledVersion + ' is already installed. Use a newer installer.';
end;

function NextButtonClick(PageID: Integer): Boolean;
var
  ExitCode: Integer;
begin
  Result := True;
  if (MaintenancePage = nil) or (PageID <> MaintenancePage.ID) then
    Exit;

  Result := (InstallAction >= 0) and (MaintenancePage.SelectedValueIndex = InstallAction);
  if Result or (UninstallAction < 0) or
     (MaintenancePage.SelectedValueIndex <> UninstallAction) then
    Exit;

  WizardForm.Hide;
  if not Exec(UninstallerPath, '', ExtractFileDir(UninstallerPath),
      SW_SHOWNORMAL, ewWaitUntilTerminated, ExitCode) then
  begin
    WizardForm.Show;
    MsgBox('Could not open the Mapper uninstaller. ' + SysErrorMessage(ExitCode), mbError, MB_OK);
    Exit;
  end;
  if RegKeyExists(HKCU, MapperUninstallKey) then
  begin
    WizardForm.Show;
    Exit;
  end;
  ExitingAfterMaintenance := True;
  WizardForm.Close;
end;

procedure CancelButtonClick(PageID: Integer; var Cancel, Confirm: Boolean);
begin
  if ExitingAfterMaintenance then
    Confirm := False;
end;
