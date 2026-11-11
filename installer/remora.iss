; Inno-Setup-Skript für Remora RDP (Windows 10 1809+ / Windows 11, x64).
;
; Bauen:  powershell -ExecutionPolicy Bypass -File installer\build.ps1
; Ergebnis: target\installer\Remora-RDP-Setup-<Version>-x64.exe
;
; Installiert nach "C:\Program Files\Remora RDP" (Administratorrechte), legt einen
; Startmenü-Eintrag, optional ein Desktop-Symbol und die .rdp-Zuordnung ("Öffnen mit") an
; und registriert die Deinstallation unter "Apps & Features".

#define AppName "Remora RDP"
#define AppExe "remora.exe"
#define ExePath "..\target\release\remora.exe"
#ifndef AppVersion
  #define AppVersion GetStringFileInfo(ExePath, "ProductVersion")
#endif
#if AppVersion == ""
  #error "target\release\remora.exe fehlt – zuerst 'cargo build --release' ausführen"
#endif

[Setup]
AppId={{CAA54C2C-CCBC-48D9-9C2A-06929D662F8C}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Remora
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\target\installer
OutputBaseFilename=Remora-RDP-Setup-{#AppVersion}-x64
SetupIconFile=..\assets\remora.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; Willkommensseite zeigen (großes Logo links)
DisableWelcomePage=no
; Assistentenbilder aus dem Logo (erzeugt von `cargo run --example make_icon`)
WizardImageFile=images\wizard-164x314.bmp,images\wizard-246x459.bmp,images\wizard-328x628.bmp
WizardSmallImageFile=images\wizard-small-55.bmp,images\wizard-small-83.bmp,images\wizard-small-110.bmp
; Laufende Remora-Instanzen vor dem Aktualisieren/Deinstallieren schließen
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes

[Languages]
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
german.AssocGroup=Dateizuordnung:
german.AssocTask=Remora unter „Öffnen mit“ für .rdp-Dateien anbieten
german.AssocName=Remotedesktopverbindung (Remora)
german.DeleteData=Sollen auch die Verbindungen, Ordner, Einstellungen und Protokolle von Remora gelöscht werden?%n%nGespeicherte Passwörter im Windows Credential Manager (Einträge „Remora RDP/…“) bleiben erhalten und können dort entfernt werden.
english.AssocGroup=File association:
english.AssocTask=Offer Remora in "Open with" for .rdp files
english.AssocName=Remote Desktop Connection (Remora)
english.DeleteData=Also delete Remora's connections, folders, settings and logs?%n%nSaved passwords in Windows Credential Manager ("Remora RDP/..." entries) are kept and can be removed there.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "rdpassoc"; Description: "{cm:AssocTask}"; GroupDescription: "{cm:AssocGroup}"

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
; Eingebettete Schrift Orbitron (SIL Open Font License 1.1) – Lizenz muss mitgeliefert werden
Source: "..\assets\fonts\OFL.txt"; DestDir: "{app}\licenses"; DestName: "Orbitron-OFL.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Remote-Desktop-Verbindungen verwalten"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; ProgID für .rdp-Dateien; ersetzt NICHT die Standardzuordnung (mstsc), sondern ergänzt "Öffnen mit".
Root: HKA; Subkey: "Software\Classes\Remora.RdpFile"; ValueType: string; ValueName: ""; ValueData: "{cm:AssocName}"; Flags: uninsdeletekey; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\Remora.RdpFile\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\Remora.RdpFile\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\.rdp\OpenWithProgids"; ValueType: string; ValueName: "Remora.RdpFile"; ValueData: ""; Flags: uninsdeletevalue; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "{#AppName}"; Flags: uninsdeletekey; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\SupportedTypes"; ValueType: string; ValueName: ".rdp"; ValueData: ""; Tasks: rdpassoc
Root: HKA; Subkey: "Software\Classes\Applications\{#AppExe}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: rdpassoc

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Automatisches Update aus Remora (/SILENT /RELAUNCH): danach wieder starten, ohne Administratorrechte
Filename: "{app}\{#AppExe}"; Flags: nowait runasoriginaluser; Check: ShouldRelaunch

[Code]
function ShouldRelaunch: Boolean;
begin
  { Nur wenn Remora das Setup selbst für ein Update gestartet hat }
  Result := WizardSilent and (Pos('/RELAUNCH', UpperCase(GetCmdTail)) > 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    { Autostart-Eintrag entfernen, den die Anwendung selbst anlegt (Einstellungen → Mit Windows starten) }
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Remora RDP Manager');
    { Benutzerdaten nur auf ausdrücklichen Wunsch löschen; Standard ist "Nein" }
    if not UninstallSilent then
      if SuppressibleMsgBox(CustomMessage('DeleteData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
      begin
        DelTree(ExpandConstant('{userappdata}\Remora'), True, True, True);
        DelTree(ExpandConstant('{localappdata}\Remora'), True, True, True);
      end;
  end;
end;
