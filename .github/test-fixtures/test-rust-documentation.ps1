#requires -Version 7.0
param([Parameter(Mandatory = $true)][string]$RepositoryRoot)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $RepositoryRoot "scripts/hooks/pre-commit.ps1") -SourceOnly

$source = "pub fn ready() -> bool { true }`n"
$cases = @(
    @('outer doc', $source, "/// Ready.`n$source", $true),
    @('inner doc', $source, "//! Readiness.`n$source", $true),
    @('replace doc', "/// Old.`n$source", "/// New.`n$source", $true),
    @('remove doc', "/// Old.`n$source", $source, $true),
    @('block doc', $source, "/** Ready.`n * Always. */`n$source", $true),
    @('nested block doc', $source, "/** Ready /* nested */. */`n$source", $true),
    @('inner block doc', $source, "/*! Ready. */`n$source", $true),
    @('mixed runtime', $source, "/// Ready.`npub fn ready() -> bool { false }`n", $false),
    @('attribute', $source, "/// Ready.`n#[inline]`n$source", $false),
    @('doc attribute', $source, "#[doc = `"Ready.`"]`n$source", $false),
    @('ordinary line', "//// Old.`n$source", "//// New.`n$source", $false),
    @('ordinary block', "/*** Old. */`n$source", "/*** New. */`n$source", $false),
    @('empty ordinary block', $source, "/**/`n$source", $false),
    @('CRLF docs', $source, ("/// Ready.`n$source").Replace("`n", "`r`n"), $true),
    @('literal Unicode normalization', 'const S: &str = "é";', ("/// Docs.`nconst S: &str = `"e" + [char]0x301 + '";'), $false),
    @('unchanged', $source, $source, $false),
    @('unterminated raw string', 'const S: &str = r#"', "const S: &str = r#`"`n/// docs`n", $false),
    @('unterminated block', '/* outer', "/* outer`n/// docs`n", $false),
    @('unterminated string', 'const S: &str = "', "const S: &str = `"`n/// docs`n", $false)
)
foreach ($prefix in @('"', 'r"', 'r#"', 'br##"', 'cr###"')) {
    $suffix = '"' + ('#' * @($prefix.ToCharArray() | Where-Object { $_ -eq '#' }).Count) + ";`n"
    $cases += ,@("literal $prefix", "const S: &str = $prefix`n/// old`n$suffix", "const S: &str = $prefix`n/// new`n$suffix", $false)
}
$cases += ,@('nested ordinary comment', "/* outer`n/* nested */`n/// old`n*/`n$source", "/* outer`n/* nested */`n/// new`n*/`n$source", $false)
$cases += ,@('character and lifetime', "fn ready<'a>() { let c = '`"'; }`n", "/// Ready.`nfn ready<'a>() { let c = '`"'; }`n", $true)

$macroDefinition = 'macro_rules! text { ($($tokens:tt)*) => { stringify!($($tokens)*) }; }'
foreach ($delimiters in @(@('{', '}'), @('(', ')'), @('[', ']'))) {
    $old = "$macroDefinition`nconst TEXT: &str = text! $($delimiters[0])`n/// OLD`nfn unused() {}`n$($delimiters[1]);`nfn main() { println!(`"{}md`", TEXT); }`n"
    $cases += ,@("macro input $($delimiters[0])", $old, $old.Replace('OLD', 'NEW'), $false)
}
foreach ($name in @('文本', '℘', 'r#type')) {
    $old = "macro_rules! $name { (`$(`$tokens:tt)*) => { stringify!(`$(`$tokens)*) }; }`nconst TEXT: &str = $name! {`n/// OLD`nfn unused() {}`n};`n"
    $cases += ,@("macro name $name", $old, $old.Replace('OLD', 'NEW'), $false)
}
$cases += ,@('macro definition', "macro_rules! made { () => {`n/// OLD`nfn made() {}`n} }", "macro_rules! made { () => {`n/// NEW`nfn made() {}`n} }", $false)
foreach ($attribute in @('capture', 'derive(Capture)', 'cfg_attr(feature = "macro", capture)', 'cfg_attr(any(), derive(Debug, Capture))')) {
    $old = "#[$attribute]`nstruct Args {`n/// OLD`nvalue: String,`n}`n"
    $cases += ,@("custom attribute $attribute", $old, $old.Replace('OLD', 'NEW'), $false)
    $old = "/// OLD`n#[$attribute]`nstruct Args { value: String }`n"
    $cases += ,@("doc before custom attribute $attribute", $old, $old.Replace('OLD', 'NEW'), $false)
}
$old = "#![capture]`nfn first() {}`n/// OLD`nfn second() {}`n"
$cases += ,@('custom inner attribute', $old, $old.Replace('OLD', 'NEW'), $false)
foreach ($declaration in @(
    'fn generic<const N: usize = { 1 + 1 }>()',
    'impl<const N: usize = { 1 + 1 }> Generic<N>',
    'trait Generic<const N: usize = { 1 + 1 }>',
    'struct Generic<const N: usize = { 1 + 1 }>',
    'enum Generic<const N: usize = { 1 + 1 }>',
    'union Generic<const N: usize = { 1 + 1 }>',
    'mod nested',
    'extern "C"'
)) {
    $old = "#[capture]`n$declaration {`n/// OLD`nfn inside() {}`n}`n"
    $cases += ,@("custom body $declaration", $old, $old.Replace('OLD', 'NEW'), $false)
    $outside = "$old`n/// OLD OUTSIDE`nfn outside() {}`n"
    $cases += ,@("outside custom body $declaration", $outside, $outside.Replace('OLD OUTSIDE', 'NEW OUTSIDE'), $true)
}
foreach ($declaration in @('const VALUE: u32 =', 'static VALUE: u32 =', 'type Value =')) {
    $old = "#[capture]`n$declaration {`n/// OLD`nfn inside() {}`n1`n};`n"
    $cases += ,@("custom expression $declaration", $old, $old.Replace('OLD', 'NEW'), $false)
    $outside = "$old`n/// OLD OUTSIDE`nfn outside() {}`n"
    $cases += ,@("outside custom expression $declaration", $outside, $outside.Replace('OLD OUTSIDE', 'NEW OUTSIDE'), $true)
}
$old = "#[derive(Clone, Debug)]`nstruct Plain {`n/// OLD`nvalue: String,`n}`n"
$cases += ,@('built-in derives', $old, $old.Replace('OLD', 'NEW'), $true)
foreach ($shadow in @('use derive_shadow_capture::Clone;', 'use derive_shadow_capture::*;', 'use derive_shadow_capture::Capture as Clone;', 'extern crate derive_shadow_capture as Clone;')) {
    $shadowedDerive = "$shadow`n$old"
    $cases += ,@("shadowed built-in derive $shadow", $shadowedDerive, $shadowedDerive.Replace('OLD', 'NEW'), $false)
}
$nestedDerive = "mod nested { use derive_shadow_capture::*;`n$old`n}`n"
$cases += ,@('nested shadowed built-in derive', $nestedDerive, $nestedDerive.Replace('OLD', 'NEW'), $false)

$old = "#[cfg_attr(all(), derive(Clone, Debug), allow(dead_code))]`nstruct Plain {`n/// OLD`nvalue: String,`n}`n"
$cases += ,@('built-in conditional attributes', $old, $old.Replace('OLD', 'NEW'), $true)
$old = "/// OLD`nfn outside() { println!(`"unrelated macro`"); }`n"
$cases += ,@('outside unrelated macro', $old, $old.Replace('OLD', 'NEW'), $true)
$old = "#[derive(Debug)]`nstruct Plain {`n/// OLD`nvalue: String,`n}`n#[cfg(test)]`nmod tests { use derive_shadow_capture::*; }`n"
$cases += ,@('test-only glob does not shadow production derive', $old, $old.Replace('OLD', 'NEW'), $true)
$old = "#[derive(Default)]`nenum Backend { #[default] One }`n/// OLD`nfn outside() {}`n"
$cases += ,@('unsupported variant attribute stays in enclosing enum', $old, $old.Replace('OLD', 'NEW'), $true)


$asyncTrait = "use async_trait::async_trait;`n#[async_trait]`ntrait Ordinary {`n/// OLD`nasync fn run(&self);`n}`n"
$cases += ,@('pinned async_trait method docs', $asyncTrait, $asyncTrait.Replace('OLD', 'NEW'), $true)
$qualifiedAsyncTrait = "#[async_trait::async_trait(?Send)]`ntrait Ordinary {`n/// OLD`nasync fn run(&self);`n}`n"
$cases += ,@('qualified pinned async_trait method docs', $qualifiedAsyncTrait, $qualifiedAsyncTrait.Replace('OLD', 'NEW'), $true)
$shadowed = "mod async_trait { pub use other::capture as async_trait; }`n$asyncTrait"
$cases += ,@('shadowed async_trait binding', $shadowed, $shadowed.Replace('OLD', 'NEW'), $false)
$aliased = $asyncTrait.Replace('use async_trait::async_trait;', 'use other::capture as async_trait;')
$cases += ,@('aliased async_trait binding', $aliased, $aliased.Replace('OLD', 'NEW'), $false)
$outerMacro = $asyncTrait.Replace('#[async_trait]', "#[capture]`n#[async_trait]")
$cases += ,@('custom attribute around async_trait', $outerMacro, $outerMacro.Replace('OLD', 'NEW'), $false)
$globShadow = "use ::async_trait::async_trait;`nmod nested {`nuse glob_capture::*;`n#[async_trait]`ntrait Ordinary {`n/// OLD`nasync fn run(&self);`n}`n}`n"
$cases += ,@('nested glob-shadowed async_trait', $globShadow, $globShadow.Replace('OLD', 'NEW'), $false)

foreach ($case in $cases) {
    $actual = Test-RustDocumentationOnlyChange -OldContent $case[1] -NewContent $case[2]
    if ($actual -ne $case[3]) { throw "Wrong classification: $($case[0]): $actual" }
}

$originalLocation = Get-Location
$originalDirectory = [Environment]::CurrentDirectory
$temporary = Join-Path ([IO.Path]::GetTempPath()) ("rustdoc-gate-" + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $temporary | Out-Null
    Set-Location -LiteralPath $temporary
    [Environment]::CurrentDirectory = $temporary
    $script:RepoRoot = $temporary
    function Git {
        param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
        $result = Invoke-Native -FileName git -Arguments $Arguments
        if ($result.ExitCode -ne 0) { throw $result.Output }
    }
    Git init -q
    Git config user.name Fixture
    Git config user.email fixture@example.invalid
    Git config commit.gpgsign false
    Git config core.hooksPath .git/empty-hooks
    Git config core.autocrlf false
    foreach ($path in @('server.rs', 'bench.rs')) { [IO.File]::WriteAllText((Join-Path $temporary $path), $source) }
    Git add .
    Git commit -qm seed
    foreach ($case in $cases | Where-Object { $_[0] -in @('outer doc', 'mixed runtime', 'attribute') }) {
        [IO.File]::WriteAllText((Join-Path $temporary 'server.rs'), $case[2])
        Git add server.rs
        foreach ($mode in @($false, $true)) {
            $script:InspectWorktree = $mode
            if ((Test-FileDiffIsTestOnly -Path server.rs) -ne $case[3]) { throw "Wrong gate: $($case[0]), Worktree=$mode" }
            $script:Failed = 0
            Test-ChangelogGate -ChangedFiles @('server.rs')
            if (($script:Failed -eq 0) -ne $case[3]) { throw "Wrong changelog verdict: $($case[0]), Worktree=$mode" }
        }
    }

    function Rustc {
        param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Arguments)
        $result = Invoke-Native -FileName rustc -Arguments $Arguments
        if ($result.ExitCode -ne 0) { throw $result.Output }
    }
    function Compile-Output {
        param([string]$Name, [string]$Content, [string[]]$Extra = @())
        [IO.File]::WriteAllText((Join-Path $temporary "$Name.rs"), $Content)
        $executable = Join-Path $temporary ($Name + $(if ($IsWindows) { '.exe' } else { '' }))
        Rustc -Arguments (@('--edition', '2021', "$Name.rs", '-o', $executable) + $Extra)
        $result = Invoke-Native -FileName $executable -Arguments @('fixture')
        if ($result.ExitCode -ne 0) { throw $result.Output }
        $result.Stdout
    }
    function Assert-Compiled-Change {
        param([string]$Name, [string]$OldContent, [string]$NewContent, [string[]]$Extra = @())
        if (Test-RustDocumentationOnlyChange -OldContent $OldContent -NewContent $NewContent) { throw "Runtime docs were exempted: $Name" }
        $before = Compile-Output -Name "${Name}_old" -Content $OldContent -Extra $Extra
        $after = Compile-Output -Name "${Name}_new" -Content $NewContent -Extra $Extra
        if ([string]::Equals($before, $after, [StringComparison]::Ordinal)) { throw "Compiled control did not change runtime output: $Name" }
        [IO.File]::WriteAllText((Join-Path $temporary 'macro.rs'), $OldContent)
        Git add macro.rs
        Git commit -qm "seed $Name"
        [IO.File]::WriteAllText((Join-Path $temporary 'macro.rs'), $NewContent)
        Git add macro.rs
        foreach ($mode in @($false, $true)) {
            $script:InspectWorktree = $mode
            if (Test-FileDiffIsTestOnly -Path macro.rs) { throw "Runtime macro docs passed git gate: $Name, Worktree=$mode" }
        }
    }
    foreach ($case in $cases | Where-Object { $_[0] -like 'macro input *' }) {
        $name = 'macro_' + [int][char]$case[0][-1]
        Assert-Compiled-Change -Name $name -OldContent $case[1] -NewContent $case[2]
    }
    $procedure = @'
extern crate proc_macro;
use proc_macro::TokenStream;
#[proc_macro_attribute]
pub fn capture(_: TokenStream, item: TokenStream) -> TokenStream {
    let generated = format!("fn captured() -> &'static str {{ {:?} }}", item.to_string());
    let mut output = item;
    output.extend(generated.parse::<TokenStream>().unwrap());
    output
}
#[proc_macro_derive(Capture)]
pub fn derive(item: TokenStream) -> TokenStream {
    format!("fn captured() -> &'static str {{ {:?} }}", item.to_string()).parse().unwrap()
}
#[proc_macro_derive(Clone)]
pub fn shadow_clone(item: TokenStream) -> TokenStream {
    derive(item)
}
'@
    [IO.File]::WriteAllText((Join-Path $temporary 'doc_capture.rs'), $procedure)
    $library = Join-Path $temporary $(if ($IsWindows) { 'doc_capture.dll' } elseif ($IsMacOS) { 'libdoc_capture.dylib' } else { 'libdoc_capture.so' })
    Rustc -Arguments @('--crate-name', 'doc_capture', '--crate-type', 'proc-macro', '--edition', '2021', 'doc_capture.rs', '-o', $library)
    foreach ($attribute in @('doc_capture::capture', 'derive(doc_capture::Capture)')) {
        $old = "#[$attribute]`nstruct Args {`n/// OLD`nvalue: String,`n}`nfn main() { println!(`"{}`", captured()); }`n"
        $name = if ($attribute.StartsWith('derive')) { 'custom_derive' } else { 'custom_attribute' }
        Assert-Compiled-Change -Name $name -OldContent $old -NewContent $old.Replace('OLD', 'NEW') -Extra @('--extern', "doc_capture=$library")
    }
    $old = "use doc_capture::Clone;`n#[derive(Clone)]`nstruct Args {`n/// OLD`nvalue: String,`n}`nfn main() { println!(`"{}`", captured()); }`n"
    Assert-Compiled-Change -Name builtin_derive_shadow -OldContent $old -NewContent $old.Replace('OLD', 'NEW') -Extra @('--extern', "doc_capture=$library")
    [IO.File]::WriteAllText((Join-Path $temporary 'server.rs'), $source)
    Git add server.rs
    Git commit -qm 'restore server baseline'
    [IO.File]::WriteAllText((Join-Path $temporary 'server.rs'), "/// Staged.`n$source")
    Git add server.rs
    [IO.File]::WriteAllText((Join-Path $temporary 'server.rs'), "pub fn ready() -> bool { false }`n")
    $script:InspectWorktree = $false
    if (-not (Test-FileDiffIsTestOnly -Path server.rs)) { throw 'Unstaged runtime edit changed staged classification' }
    $script:InspectWorktree = $true
    if (Test-FileDiffIsTestOnly -Path server.rs) { throw 'Worktree runtime edit was ignored' }
    $cachePaths = @('cache-a.rs', 'cache space.rs', 'cache-雪.rs')
    foreach ($path in $cachePaths) { [IO.File]::WriteAllText((Join-Path $temporary $path), $source) }
    Git add -- @cachePaths
    Git commit -qm 'seed cached source blobs'
    foreach ($path in $cachePaths) { [IO.File]::WriteAllText((Join-Path $temporary $path), "/// Cached docs.`n$source") }
    Git add -- @cachePaths
    foreach ($mode in @($false, $true)) {
        $script:InspectWorktree = $mode
        $cache = Get-ChangelogRustTextCache -Paths $cachePaths
        if ($cache.Count -ne 3) { throw "Wrong cached path count: $($cache.Count)" }
        foreach ($path in $cachePaths) {
            if (-not (Test-FileDiffIsTestOnly -Path $path -CachedContent $cache[$path])) { throw "Cached docs rejected: $path, Worktree=$mode" }
        }
    }
    [IO.File]::WriteAllText((Join-Path $temporary $cachePaths[1]), "/// Mixed docs.`npub fn ready() -> bool { false }`n")
    Git add -- $cachePaths[1]
    $script:InspectWorktree = $false
    $cache = Get-ChangelogRustTextCache -Paths $cachePaths
    if (Test-FileDiffIsTestOnly -Path $cachePaths[1] -CachedContent $cache[$cachePaths[1]]) { throw 'Cached runtime edit was exempted' }
    Git update-index --chmod=+x -- $cachePaths[0]
    $cache = Get-ChangelogRustTextCache -Paths $cachePaths
    if (Test-FileDiffIsTestOnly -Path $cachePaths[0] -CachedContent $cache[$cachePaths[0]]) { throw 'Cached executable mode change was exempted' }
} finally {
    Set-Location $originalLocation
    [Environment]::CurrentDirectory = $originalDirectory
    Remove-Item -LiteralPath $temporary -Recurse -Force
}
Write-Output 'Rust documentation fixtures pass'
