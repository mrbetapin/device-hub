param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../dist')
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$plugin = Join-Path $OutputDirectory 'devhub_obs'
$bin = Join-Path $plugin 'bin/64bit'
$locale = Join-Path $plugin 'data/locale'
$licenses = Join-Path $OutputDirectory 'licenses'
$pluginLicenses = Join-Path $plugin 'licenses'

New-Item -ItemType Directory -Force -Path $bin, $locale, $licenses, $pluginLicenses | Out-Null
Copy-Item -LiteralPath (Join-Path $root 'target/release/device-hub.exe') -Destination $OutputDirectory
# 常駐エージェント (Logitech HID++ の再適用)。device-hub.exe と同じフォルダに置く前提。
Copy-Item -LiteralPath (Join-Path $root 'target/release/devhub-agent.exe') -Destination $OutputDirectory
Copy-Item -LiteralPath (Join-Path $root 'target/release/devhub_obs.dll') -Destination $bin
Copy-Item -Path (Join-Path $root 'crates/devhub-obs/data/locale/*.ini') -Destination $locale

foreach ($destination in $OutputDirectory, $plugin) {
    Copy-Item -LiteralPath (Join-Path $root 'docs/QUICKSTART_JA.md') -Destination $destination
}

foreach ($name in 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md') {
    $source = Join-Path $root $name
    Copy-Item -LiteralPath $source -Destination $OutputDirectory
    Copy-Item -LiteralPath $source -Destination $plugin
}

Copy-Item -Path (Join-Path $root 'licenses/*') -Destination $licenses
foreach ($name in 'gpui-pre', 'gpui-pre-windows') {
    Copy-Item -LiteralPath (Join-Path $root "vendor/$name/LICENSE-APACHE") `
        -Destination (Join-Path $licenses "$name-LICENSE-APACHE")
}
Copy-Item -Path (Join-Path $licenses '*') -Destination $pluginLicenses
