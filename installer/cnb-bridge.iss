#define AppName "CNB Bridge"
#define AppVersion "0.1.0"
#define AppPublisher "CNB Bridge"
#define AppExeName "cnb-bridge.exe"

[Setup]
AppId={{7D6E6A92-3C6D-4B5E-BF1F-0C0B1D6E0001}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\Programs\CNB Bridge
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=output
OutputBaseFilename=cnb-bridge-{#AppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\{#AppExeName}
SetupIconFile=..\src-tauri\icons\icon.ico

[Languages]
Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExeName}"; Description: "启动 {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
function WebView2Installed(): Boolean;
var
  Version: String;
begin
  Result :=
    RegQueryStringValue(HKLM, 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F1E7E0FE-0E4A-4B4C-9C9A-2B0F0F3C1D33}', 'pv', Version) or
    RegQueryStringValue(HKCU, 'Software\Microsoft\EdgeUpdate\Clients\{F1E7E0FE-0E4A-4B4C-9C9A-2B0F0F3C1D33}', 'pv', Version);
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not WebView2Installed() then
  begin
    if MsgBox('未检测到 Microsoft Edge WebView2 Runtime。CNB Bridge 界面需要 WebView2，是否继续安装？' + #13#10 + #13#10 + '可从 Microsoft 官网安装 Evergreen WebView2 Runtime。', mbConfirmation, MB_YESNO) = IDNO then
      Result := False;
  end;
end;
