; Inno Setup script for WebDock Dev (Windows).
; Build with: .\build-installer.ps1   (requires Inno Setup 6: winget install JRSoftware.InnoSetup)

#define AppName "WebDock Dev"
#define AppExe "webdock-dev.exe"
#define AppVersion "0.1.0"

[Setup]
AppId={{6F2B5C7E-3B0F-4C57-9A5E-4E0A1B7D2C11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=WebDock Dev
DefaultDirName={localappdata}\Programs\WebDockDev
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=WebDockDev-Setup
SetupIconFile=..\assets\app.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "autostart"; Description: "Start WebDock Dev when I sign in to Windows"; GroupDescription: "Startup:"
Name: "mkcert"; Description: "Install mkcert (browser-trusted HTTPS certificates, via winget)"; GroupDescription: "Optional components:"; Flags: unchecked

[InstallDelete]
; Leftovers from the product's former name (Local Dev Portal).
Type: files; Name: "{app}\local-dev-portal.exe"
Type: files; Name: "{autoprograms}\Local Dev Portal.lnk"
Type: files; Name: "{autodesktop}\Local Dev Portal.lnk"

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "LocalDevPortal"; Flags: deletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "WebDockDev"; ValueData: """{app}\{#AppExe}"" --background"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{cmd}"; Parameters: "/C winget install -e --id FiloSottile.mkcert --accept-source-agreements --accept-package-agreements"; Tasks: mkcert; Flags: runhidden waituntilterminated; StatusMsg: "Installing mkcert..."
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent

[Code]
var
  SitesPage: TInputDirWizardPage;
  DockerPage: TWizardPage;
  DockerFound: Boolean;

function DetectDocker: Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'), '/C docker --version', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

procedure OpenDockerSite(Sender: TObject);
var
  Code: Integer;
begin
  ShellExec('open', 'https://www.docker.com/products/docker-desktop/', '', '', SW_SHOWNORMAL, ewNoWait, Code);
end;

procedure InitializeWizard;
var
  Msg: TNewStaticText;
  Btn: TNewButton;
  Existing: AnsiString;
  Dflt: String;
begin
  Dflt := ExpandConstant('{userdocs}\WebDockDev\sites');
  if LoadStringFromFile(ExpandConstant('{localappdata}\WebDockDev\sites_dir.txt'), Existing) then
    if Trim(String(Existing)) <> '' then Dflt := Trim(String(Existing));
  SitesPage := CreateInputDirPage(wpSelectDir, 'Website folder',
    'Where should your websites be stored?',
    'Each project gets its own sub-folder here. You can change this location now; it can be any folder on your PC.',
    False, '');
  SitesPage.Add('');
  SitesPage.Values[0] := Dflt;

  DockerFound := DetectDocker;
  DockerPage := CreateCustomPage(wpSelectTasks, 'Docker Desktop',
    'WebDock Dev runs your websites in Docker containers.');

  Msg := TNewStaticText.Create(DockerPage);
  Msg.Parent := DockerPage.Surface;
  Msg.AutoSize := False;
  Msg.WordWrap := True;
  Msg.Width := DockerPage.SurfaceWidth;
  Msg.Height := ScaleY(90);
  if DockerFound then
    Msg.Caption := 'Docker was found on this computer. You are all set.'
  else
    Msg.Caption := 'Docker Desktop was not found. Install it, start it once, and WebDock Dev sets up ' +
      'everything else itself (web server, PHP versions, database, phpMyAdmin).' + #13#10#13#10 +
      'You can continue with this installation now and install Docker afterwards; the portal dashboard shows what to do.';

  if not DockerFound then
  begin
    Btn := TNewButton.Create(DockerPage);
    Btn.Parent := DockerPage.Surface;
    Btn.Top := Msg.Top + Msg.Height;
    Btn.Width := ScaleX(240);
    Btn.Height := ScaleY(25);
    Btn.Caption := 'Open the Docker Desktop download page';
    Btn.OnClick := @OpenDockerSite;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  // Make sure a running instance does not block replacing the executable.
  Exec(ExpandConstant('{cmd}'), '/C taskkill /IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{cmd}'), '/C taskkill /IM local-dev-portal.exe /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Dir: String;
begin
  if CurStep = ssInstall then
  begin
    Dir := ExpandConstant('{localappdata}\WebDockDev');
    ForceDirectories(Dir);
    ForceDirectories(SitesPage.Values[0]);
    SaveStringToFile(Dir + '\sites_dir.txt', SitesPage.Values[0], False);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    Exec(ExpandConstant('{cmd}'), '/C taskkill /IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
    // The tray menu can also create the autostart entry.
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'WebDockDev');
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'LocalDevPortal');
  end;
end;
