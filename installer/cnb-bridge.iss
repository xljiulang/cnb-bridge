#define MyAppName "CNB Bridge"
#define MyAppExeName "cnb-bridge.exe"
#define MyAppGuid "{7D6E6A92-3C6D-4B5E-BF1F-0C0B1D6E0001}"
#define AppMutexName "Local\cool.cnb.bridge.desktop"

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif
#ifndef ReadmePath
  #define ReadmePath "..\README.md"
#endif
#ifndef AppIconPath
  #define AppIconPath "assets\cnb-bridge.ico"
#endif
#ifndef OutputDir
  #define OutputDir "output"
#endif
#ifndef ArchitectureMode
  #define ArchitectureMode "x64compatible"
#endif

[Setup]
AppId={{#MyAppGuid}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppName}
VersionInfoCompany={#MyAppName}
VersionInfoDescription={#MyAppName}
VersionInfoProductName={#MyAppName}
VersionInfoVersion={#MyAppVersion}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
UsePreviousAppDir=yes
OutputDir={#OutputDir}
OutputBaseFilename=cnb-bridge-v{#MyAppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=no
RestartApplications=no
RestartIfNeededByRun=no
MinVersion=10.0
ArchitecturesAllowed={#ArchitectureMode}
ArchitecturesInstallIn64BitMode={#ArchitectureMode}
SetupIconFile={#AppIconPath}
UninstallDisplayIcon={app}\{#MyAppExeName}

[Languages]
Name: "chinesesimp"; MessagesFile: "languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加图标："; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "runtime\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ReadmePath}"; DestDir: "{app}"; DestName: "README.md"; Flags: ignoreversion
Source: "{#AppIconPath}"; DestDir: "{app}"; DestName: "app.ico"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; IconFilename: "{app}\app.ico"
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"; IconFilename: "{app}\app.ico"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; WorkingDir: "{app}"; IconFilename: "{app}\app.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "启动 {#MyAppName}"; Flags: nowait postinstall skipifsilent; Check: ShouldOfferLaunch

[Code]
var
  AppWasRunning: Boolean;

function IsApplicationRunning: Boolean;
begin
  Result := CheckForMutexes('{#AppMutexName}');
end;

function StopRunningApplication: String;
var
  ExePath: String;
  ResultCode: Integer;
  Index: Integer;
begin
  Result := '';
  AppWasRunning := IsApplicationRunning;
  ExePath := ExpandConstant('{app}\{#MyAppExeName}');
  if FileExists(ExePath) then
    Exec(ExePath, '--shutdown-for-update', ExpandConstant('{app}'), SW_HIDE, ewNoWait, ResultCode);

  if AppWasRunning then
  begin
    for Index := 1 to 50 do
    begin
      if not IsApplicationRunning then
        Break;
      Sleep(200);
    end;
  end;

  { Older releases do not create the lifecycle mutex, so always issue a
    process-name-limited fallback after the graceful-shutdown request. }
  ResultCode := -1;
  Exec(
    ExpandConstant('{sysnative}\taskkill.exe'),
    '/IM {#MyAppExeName} /T /F',
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode
  );
  if ResultCode = 0 then
    AppWasRunning := True;

  for Index := 1 to 25 do
  begin
    if not IsApplicationRunning then
      Exit;
    Sleep(200);
  end;

  Result := '无法关闭正在运行的 {#MyAppName}。请从系统托盘退出应用后重试。';
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := StopRunningApplication;
end;

function ShouldOfferLaunch: Boolean;
begin
  Result := not AppWasRunning;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if (CurStep = ssPostInstall) and AppWasRunning then
    Exec(
      ExpandConstant('{app}\{#MyAppExeName}'),
      '',
      ExpandConstant('{app}'),
      SW_SHOWNORMAL,
      ewNoWait,
      ResultCode
    );
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  StopError: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;

  StopError := StopRunningApplication;
  if StopError <> '' then
  begin
    MsgBox(StopError, mbError, MB_OK);
    Abort;
  end;

  RegDeleteValue(
    HKCU,
    'Software\Microsoft\Windows\CurrentVersion\Run',
    '{#MyAppName}'
  );
  RegDeleteValue(
    HKCU,
    'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run',
    '{#MyAppName}'
  );
end;
