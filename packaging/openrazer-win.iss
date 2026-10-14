; Inno Setup script for openrazer-win. Build with tools\build-release.ps1, which
; passes /DAppVersion=<workspace version>.
;
; System-wide install into Program Files (admin). Autostart is per user (HKCU Run,
; owned by the app's "Start with Windows" switch), so the installer sets it by
; running the tray as the signed-in user rather than writing the registry elevated.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z (tools\build-release.ps1 does this)
#endif

#define AppName "openrazer-win"
#define TrayExe "razer-tray.exe"

[Setup]
; Never change AppId: upgrades and uninstall are keyed on it.
AppId={{4B38C3DC-0C51-4178-9970-59B778562074}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppName}
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=openrazer.ico
UninstallDisplayIcon={app}\{#TrayExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; Remember the autostart checkbox across upgrades.
UsePreviousTasks=yes
; We stop the tray ourselves (it has no visible window to close politely).
CloseApplications=no

[Tasks]
Name: "autostart"; Description: "Start {#AppName} when I sign in"; GroupDescription: "Startup:"

[Files]
Source: "..\target\release\{#TrayExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\razerctl.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#TrayExe}"; Comment: "Razer device settings without Synapse"

[Run]
; runasoriginaluser: HKCU of the person who ran setup, not the elevated context.
Filename: "{app}\{#TrayExe}"; Parameters: "--enable-autostart"; Flags: runasoriginaluser runhidden waituntilterminated; Tasks: autostart
Filename: "{app}\{#TrayExe}"; Parameters: "--disable-autostart"; Flags: runasoriginaluser runhidden waituntilterminated; Tasks: not autostart
Filename: "{app}\{#TrayExe}"; Description: "Start {#AppName} now"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM {#TrayExe}"; Flags: runhidden waituntilterminated; RunOnceId: "StopTray"
; Uninstall runs elevated; for an admin user that is the same HKCU.
Filename: "{app}\{#TrayExe}"; Parameters: "--disable-autostart"; Flags: runhidden waituntilterminated; RunOnceId: "DisableAutostart"

[Code]
// Stop any running tray (installed or a dev build: the single-instance mutex
// would otherwise keep the new one from starting) before files are replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#TrayExe}', '', SW_HIDE,
    ewWaitUntilTerminated, Code);
  Result := '';
end;
