# 打一份 Windows 安装包（未签名，本机自用）。build-dmg.sh 的对照物。
#
#   powershell -ExecutionPolicy Bypass -File desktop\scripts\build-windows.ps1
#   powershell -ExecutionPolicy Bypass -File desktop\scripts\build-windows.ps1 -Open
#   powershell -ExecutionPolicy Bypass -File desktop\scripts\build-windows.ps1 -Bundles nsis,msi
#
# 做的事：前端 typecheck + vite build（tauri 的 beforeBuildCommand）→ cargo release 编译 →
# 只出 nsis（默认）。产物在 desktop\target\release\bundle\nsis\。
# 首次或清过 target 后要编全量 Rust，十几分钟；之后是增量。
[CmdletBinding()]
param(
    # 默认只出 NSIS：它是对外主渠道，也是 updater 在 Windows 上认的那个。
    # 要 MSI（企业批量分发常要求）就显式加上。
    [string[]] $Bundles = @('nsis'),
    [switch] $Open
)

$ErrorActionPreference = 'Stop'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$desktop = Split-Path -Parent $here
$app = Join-Path $desktop 'apps\desktop'

function Need($name) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        Write-Error "缺 $name"
        exit 2
    }
}
Need node
Need npm
Need cargo

# 磁盘：release 增量一般要 2–3 GB 余量，低于 4 GB 先提醒。
$drive = (Get-Item $desktop).PSDrive
if ($null -ne $drive.Free -and $drive.Free -lt 4GB) {
    Write-Error ("磁盘只剩 {0:N1} GB，release 构建可能中途失败；先清 desktop\target\debug 或别处再来。" -f ($drive.Free / 1GB))
    exit 3
}

if (-not (Test-Path (Join-Path $app 'node_modules'))) {
    Push-Location $app
    npm install
    Pop-Location
}

$joined = $Bundles -join ','
Write-Host "▶ tauri build --bundles $joined（$(Get-Date -Format 'HH:mm:ss')）"
Push-Location $app
try {
    npm run build -- --bundles $joined
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}

# NSIS 出 .exe，MSI 出 .msi，两个目录分开放。
$artifacts = @(
    (Join-Path $desktop 'target\release\bundle\nsis\*.exe'),
    (Join-Path $desktop 'target\release\bundle\msi\*.msi')
) | ForEach-Object { Get-ChildItem $_ -ErrorAction SilentlyContinue } |
    Sort-Object LastWriteTime -Descending

if (-not $artifacts) {
    Write-Error '没找到安装包产物，看上面的构建输出。'
    exit 1
}

Write-Host ''
foreach ($a in $artifacts) {
    Write-Host "✔ $($a.FullName)"
    Write-Host ("  {0:N1} MB，{1:yyyy-MM-dd HH:mm:ss}" -f ($a.Length / 1MB), $a.LastWriteTime)
}
Write-Host '  未签名：SmartScreen 会拦一下，点「更多信息」→「仍要运行」。'

if ($Open) {
    Start-Process explorer.exe "/select,$($artifacts[0].FullName)"
}
