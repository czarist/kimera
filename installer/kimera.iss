; Inno Setup script for the Kimera Windows installer.
; Build: run scripts\package.ps1 first, then compile this file with Inno Setup 6:
;   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DVersion=0.2.0 installer\kimera.iss
; The installer goes to dist\.

#ifndef Version
  #define Version "0.2.0"
#endif
#define Src "..\dist\Kimera-" + Version + "-windows-x64"

[Setup]
AppId={{6F0B7C55-2D7E-4E0B-9C49-6B1E2A6A9C3D}
AppName=Kimera
AppVersion={#Version}
AppPublisher=Kimera contributors
DefaultDirName={autopf}\Kimera
DefaultGroupName=Kimera
OutputDir=..\dist
OutputBaseFilename=Kimera-{#Version}-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
LicenseFile={#Src}\LICENSE-MIT
UninstallDisplayIcon={app}\kimera-gui.exe
WizardStyle=modern

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "foldermenu"; Description: "Add ""Open in Kimera"" to the folder right-click menu"; GroupDescription: "Explorer:"
Name: "addtopath"; Description: "Add the kimera command to PATH"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#Src}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion

[Icons]
Name: "{group}\Kimera"; Filename: "{app}\kimera-gui.exe"
Name: "{group}\Recipes"; Filename: "{app}\recipes"
Name: "{group}\{cm:UninstallProgram,Kimera}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Kimera"; Filename: "{app}\kimera-gui.exe"; Tasks: desktopicon

[Registry]
; Right-click a photo folder -> Open in Kimera (kimera-gui.exe takes the folder as argument).
Root: HKA; Subkey: "Software\Classes\Directory\shell\Kimera"; ValueType: string; ValueName: ""; ValueData: "Open in Kimera"; Tasks: foldermenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\shell\Kimera"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\kimera-gui.exe"; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Kimera\command"; ValueType: string; ValueName: ""; ValueData: """{app}\kimera-gui.exe"" ""%1"""; Tasks: foldermenu
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\kimera-gui.exe"; Description: "{cm:LaunchProgram,Kimera}"; Flags: nowait postinstall skipifsilent

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;
