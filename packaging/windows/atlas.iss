#define AppName "Atlas"
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{A66B9F93-8F34-4D8A-8D6A-A71A50000001}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Atlas
DefaultDirName={autopf}\Atlas
DefaultGroupName=Atlas
DisableProgramGroupPage=yes
OutputDir=..\..\target\installer
OutputBaseFilename=AtlasSetup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\icons\atlas.ico
UninstallDisplayIcon={app}\atlas.exe
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "french"; MessagesFile: "compiler:Languages\French.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "dutch"; MessagesFile: "compiler:Languages\Dutch.isl"
Name: "turkish"; MessagesFile: "compiler:Languages\Turkish.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "cli"; Description: "Enable the atlas command in Terminal"; GroupDescription: "Personalize Atlas:"
Name: "shell"; Description: "Add Open with Atlas to Explorer"; GroupDescription: "Personalize Atlas:"; Flags: checkedonce
Name: "searchservice"; Description: "Enable ultra-fast background search service (instant NTFS indexing)"; GroupDescription: "Personalize Atlas:"; Flags: checkedonce
Name: "launch"; Description: "Launch Atlas after install"; GroupDescription: "Finish:"; Flags: checkedonce

[Files]
Source: "..\..\target\release\atlas.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\windows-runtime\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\windows-runtime\MICROSOFT-RUNTIME*"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\atlas-cloud-extension.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\release\atlas-icon-extension.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\legal\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\crates\atlas-core\vendor\unrar-bin\UnRAR.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\extensions\cloud\rclone.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\target\extensions\cloud\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; DestName: "CLOUD-THIRD-PARTY-NOTICES.txt"; Flags: ignoreversion
Source: "..\..\target\release\atlas-search-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\icons\atlas.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Atlas"; Filename: "{app}\atlas.exe"; IconFilename: "{app}\atlas.ico"
Name: "{autodesktop}\Atlas"; Filename: "{app}\atlas.exe"; IconFilename: "{app}\atlas.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Atlas"; ValueType: string; ValueName: ""; ValueData: "Open with Atlas"; Tasks: shell; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Atlas"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\atlas.exe,0"; Tasks: shell
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Atlas\command"; ValueType: string; ValueName: ""; ValueData: """{app}\atlas.exe"" ""%1"""; Tasks: shell
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Atlas"; ValueType: string; ValueName: ""; ValueData: "Open with Atlas"; Tasks: shell; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Atlas"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\atlas.exe,0"; Tasks: shell
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Atlas\command"; ValueType: string; ValueName: ""; ValueData: """{app}\atlas.exe"" ""%V"""; Tasks: shell
Root: HKCU; Subkey: "Software\Classes\Applications\atlas.exe"; ValueType: string; ValueName: "FriendlyAppName"; ValueData: "Atlas"; Tasks: shell; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\atlas.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\atlas.exe"" ""%1"""; Tasks: shell

[Run]
Filename: "{app}\atlas-search-service.exe"; Parameters: "--install"; Flags: runhidden; Tasks: searchservice; StatusMsg: "Configuring Atlas Search Service..."
Filename: "{app}\atlas.exe"; Description: "Launch Atlas"; Flags: nowait postinstall skipifsilent; Tasks: launch

[UninstallRun]
Filename: "{app}\atlas-search-service.exe"; Parameters: "--uninstall"; Flags: runhidden


[Code]
const
  EnvKey = 'Environment';

function PathPartExists(PathValue, Dir: String): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(PathValue) + ';') > 0;
end;

procedure AppendUserPath(Dir: String);
var
  PathValue: String;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', PathValue) then
    PathValue := '';

  if PathPartExists(PathValue, Dir) then
    exit;

  if PathValue = '' then
    RegWriteStringValue(HKCU, EnvKey, 'Path', Dir)
  else
    RegWriteStringValue(HKCU, EnvKey, 'Path', PathValue + ';' + Dir);
end;

procedure RemoveUserPath(Dir: String);
var
  PathValue: String;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', PathValue) then
    exit;

  StringChangeEx(PathValue, ';' + Dir + ';', ';', True);
  StringChangeEx(PathValue, Dir + ';', '', True);
  StringChangeEx(PathValue, ';' + Dir, '', True);

  if PathValue = Dir then
    PathValue := '';

  RegWriteStringValue(HKCU, EnvKey, 'Path', PathValue);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('cli') then
    AppendUserPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveUserPath(ExpandConstant('{app}'));
end;
