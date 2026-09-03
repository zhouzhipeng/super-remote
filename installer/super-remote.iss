#ifndef SourceDir
  #error SourceDir must point to the staged production directory
#endif
#ifndef OutputDir
  #error OutputDir must point to the installer output directory
#endif
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "SuperRemote-Setup"
#endif

[Setup]
AppId={{38DC5B0E-9E22-4D88-B96C-811DC75FB2A4}
AppName=Super Remote
AppVersion={#AppVersion}
AppVerName=Super Remote {#AppVersion}
AppPublisher=Super Remote
DefaultDirName={autopf}\Super Remote
DefaultGroupName=Super Remote
DisableProgramGroupPage=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
SetupLogging=yes
CloseApplications=yes
RestartApplications=no
UninstallDisplayIcon={app}\super-remote.exe
UninstallDisplayName=Super Remote

[Dirs]
Name: "{commonappdata}\Super Remote"

[Files]
Source: "{#SourceDir}\super-remote.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\remote-signaling.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\remote-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\remote-control-panel.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\remote-turn.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\FFMPEG-LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\FFMPEG-README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\PACKAGE_README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#SourceDir}\production-manifest.json"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{commonprograms}\Super Remote"; Filename: "{app}\super-remote.exe"; WorkingDir: "{app}"
Name: "{commondesktop}\Super Remote"; Filename: "{app}\super-remote.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加快捷方式："; Flags: unchecked

[Run]
Filename: "{app}\super-remote.exe"; Description: "启动 Super Remote"; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\super-remote.exe"; Parameters: "--uninstall"; WorkingDir: "{app}"; Flags: runhidden waituntilterminated skipifdoesntexist; RunOnceId: "StopSuperRemote"

[UninstallDelete]
Type: filesandordirs; Name: "{commonappdata}\Super Remote"
