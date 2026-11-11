; Edgehop installer for Windows (Inno Setup 6).
;   iscc /DAppVersion=0.3.0 /DExe=path\to\edgehop.exe /DIcon=path\to\edgehop.ico edgehop.iss
; Installs for the current user (no admin), to %LOCALAPPDATA%\Programs\Edgehop,
; with Start menu and desktop icons, start at sign-in (in the tray, through Task
; Scheduler), and optionally the Windows Firewall rule (that step asks for admin).

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Exe
  #define Exe "..\..\target\release\edgehop.exe"
#endif
#ifndef Icon
  #define Icon "edgehop.ico"
#endif

[Setup]
AppId={{6E2A4F1C-3B8D-4E57-9A21-0C5D7B3E9F42}
AppName=Edgehop
AppVersion={#AppVersion}
AppVerName=Edgehop {#AppVersion}
AppPublisher=Edgehop
AppPublisherURL=https://github.com/JawshTheDark/edgehop
AppSupportURL=https://github.com/JawshTheDark/edgehop/issues
AppUpdatesURL=https://github.com/JawshTheDark/edgehop/releases
AppComments=Share one keyboard and mouse across your computers
DefaultDirName={localappdata}\Programs\Edgehop
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputBaseFilename=Edgehop-Setup-{#AppVersion}
SetupIconFile={#Icon}
UninstallDisplayIcon={app}\edgehop.exe
UninstallDisplayName=Edgehop
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Edgehop is stopped in PrepareToInstall below.
CloseApplications=no

[Tasks]
Name: "desktopicon"; Description: "Put an Edgehop icon on the desktop"
Name: "autostart"; Description: "Start Edgehop in the tray when I sign in"
Name: "firewall"; Description: "Let other computers connect (adds a Windows Firewall rule for private networks; asks for admin once)"

[Files]
Source: "{#Exe}"; DestDir: "{app}"; DestName: "edgehop.exe"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Edgehop"; Filename: "{app}\edgehop.exe"; Comment: "Share your keyboard and mouse with your other computers"
Name: "{userdesktop}\Edgehop"; Filename: "{app}\edgehop.exe"; Comment: "Share your keyboard and mouse with your other computers"; Tasks: desktopicon

[Run]
; Start at sign-in (a Task Scheduler task; also removes the old registry entry).
Filename: "{app}\edgehop.exe"; Parameters: "--autostart on"; StatusMsg: "Setting Edgehop to start when you sign in..."; Flags: runhidden waituntilterminated; Tasks: autostart
Filename: "{app}\edgehop.exe"; Parameters: "--autostart off"; Flags: runhidden waituntilterminated; Tasks: not autostart
; Windows Firewall rule (needs admin: Windows asks once).
Filename: "{app}\edgehop.exe"; Parameters: "--firewall on"; StatusMsg: "Allowing Edgehop through Windows Firewall..."; Flags: runhidden shellexec waituntilterminated; Verb: runas; Tasks: firewall
; Afterwards: open it (or, for a silent install such as an update, start it in the tray).
Filename: "{app}\edgehop.exe"; Description: "Open Edgehop"; Flags: postinstall nowait skipifsilent
Filename: "{app}\edgehop.exe"; Parameters: "--minimized --restarting"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im edgehop.exe"; Flags: runhidden; RunOnceId: "StopEdgehop"
Filename: "{app}\edgehop.exe"; Parameters: "--autostart off"; Flags: runhidden waituntilterminated; RunOnceId: "NoAutostart"
Filename: "{app}\edgehop.exe"; Parameters: "--firewall off"; Flags: runhidden shellexec waituntilterminated; Verb: runas; RunOnceId: "NoFirewall"; Tasks: firewall

[Code]
// Stop a running Edgehop (and its predecessor, kvmlink) so its files can be replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im edgehop.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im kvmlink.exe', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Sleep(500);
  Result := '';
end;
