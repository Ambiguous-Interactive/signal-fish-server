#requires -Version 7.0

function Get-InternalRefactorExemptions {
    param(
        [ValidateSet("Staged", "Worktree", "Comparison")][string]$Mode = "Staged",
        [string]$DiffBase = "",
        [switch]$ChangeAlreadyObserved
    )

    $declarationPath = ".github/internal-refactor.json"
    $diffArguments = @("diff", "--name-only", "-z")
    $comparisonBase = $null
    $comparisonHead = $null
    if ($Mode -eq "Staged") {
        $diffArguments += @("--cached", "HEAD")
    } elseif ($Mode -eq "Worktree") {
        $diffArguments += "HEAD"
    } else {
        if ([string]::IsNullOrWhiteSpace($DiffBase)) { return ,([string[]]@()) }
        if ($DiffBase.Contains("...")) {
            $revisions = $DiffBase.Split("...", [System.StringSplitOptions]::None)
            if ($revisions.Count -ne 2) { throw "Invalid refactor comparison range" }
            $baseResult = Invoke-Native -FileName "git" -Arguments @("merge-base", $revisions[0], $revisions[1])
            $headResult = Invoke-Native -FileName "git" -Arguments @("rev-parse", "--verify", "$($revisions[1])^{commit}")
            $checkout = Invoke-Native -FileName "git" -Arguments @("rev-parse", "HEAD")
            if ($headResult.ExitCode -ne 0 -or $checkout.ExitCode -ne 0 -or $headResult.Stdout -cne $checkout.Stdout) {
                throw "Refactor comparison must end at the checked-out HEAD"
            }
            $comparisonHead = $headResult.Stdout.Trim()
        } else {
            $baseResult = Invoke-Native -FileName "git" -Arguments @("rev-parse", "--verify", "$DiffBase^{commit}")
        }
        if ($baseResult.ExitCode -ne 0) { throw "Cannot resolve refactor comparison base" }
        $comparisonBase = $baseResult.Stdout.Trim()
        $diffArguments += $DiffBase
    }
    $changed = [bool]$ChangeAlreadyObserved
    if ($ChangeAlreadyObserved -and $Mode -eq "Comparison") {
        throw "Comparison mode must discover its own declaration change"
    }
    if (-not $changed) {
        $diff = Invoke-Native -FileName "git" -Arguments ($diffArguments + @("--", $declarationPath))
        if ($diff.ExitCode -ne 0) { throw "Cannot inspect internal-refactor declaration changes" }
        $changed = -not [string]::IsNullOrEmpty($diff.Stdout)
        if ($Mode -eq "Worktree" -and -not $changed) {
            $untracked = Invoke-Native -FileName "git" -Arguments @("ls-files", "--others", "--exclude-standard", "--", $declarationPath)
            if ($untracked.ExitCode -ne 0) { throw "Cannot inspect untracked declaration" }
            $changed = -not [string]::IsNullOrEmpty($untracked.Stdout)
        }
    }
    if (-not $changed) { return ,([string[]]@()) }

    if ($null -ne $comparisonHead) {
        $declaredEntry = Invoke-Native -FileName "git" -Arguments @("ls-tree", "-z", $comparisonHead, "--", $declarationPath)
        if ($declaredEntry.ExitCode -ne 0 -or
            ($declaredEntry.Stdout.Length -gt 0 -and $declaredEntry.Stdout -cnotmatch '^100(644|755) blob [0-9a-f]{40}\t\.github/internal-refactor\.json\x00$')) {
            throw "Compared internal-refactor declaration is not one regular file"
        }
        $clean = Invoke-Native -FileName "git" -Arguments @("diff", "--quiet", $comparisonHead, "--", $declarationPath)
        if ($clean.ExitCode -ne 0) { throw "Refactor declaration differs from the compared commit" }
    }

    if ($Mode -eq "Staged") {
        $indexEntry = Invoke-Native -FileName "git" -Arguments @("ls-files", "--stage", "-z", "--", $declarationPath)
        if ($indexEntry.ExitCode -ne 0) { throw "Cannot inspect declaration file mode" }
        if ($indexEntry.Stdout.Length -gt 0 -and $indexEntry.Stdout -cnotmatch '^100(644|755) [0-9a-f]{40} 0\t\.github/internal-refactor\.json\x00$') {
            throw "Internal-refactor declaration is not one regular staged file"
        }
        $blob = Invoke-Native -FileName "git" -Arguments @("show", ":$declarationPath")
        if ($blob.ExitCode -ne 0) { return ,([string[]]@()) }
        $text = $blob.Stdout
    } else {
        $declarationItem = Get-Item -LiteralPath $declarationPath -Force -ErrorAction SilentlyContinue
        if ($null -eq $declarationItem) { return ,([string[]]@()) }
        if ($declarationItem.PSIsContainer -or ($declarationItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
            throw "Internal-refactor declaration is not a regular file"
        }
        $text = [System.IO.File]::ReadAllText((Join-Path (Get-Location) $declarationPath))
    }
    if ($text.Length -gt 65536) { throw "Internal-refactor declaration is too large" }
    $declaration = ConvertFrom-Json -InputObject $text -AsHashtable
    if ($declaration -isnot [System.Collections.IDictionary] -or
        ($declaration.Keys | Sort-Object) -join "," -cne "base,files,reason" -or
        $declaration.base -isnot [string] -or $declaration.base -cnotmatch '^[0-9a-f]{40}$' -or
        $declaration.reason -isnot [string] -or [string]::IsNullOrWhiteSpace($declaration.reason) -or
        $declaration.files -isnot [array] -or $declaration.files.Count -eq 0 -or $declaration.files.Count -gt 32) {
        throw "Invalid internal-refactor declaration schema"
    }
    $ancestor = Invoke-Native -FileName "git" -Arguments @("merge-base", "--is-ancestor", $declaration.base, "HEAD")
    if ($ancestor.ExitCode -ne 0) { throw "Internal-refactor base is not an ancestor of HEAD" }
    if ($null -ne $comparisonBase -and $comparisonBase -cne $declaration.base) {
        $baseAncestor = Invoke-Native -FileName "git" -Arguments @("merge-base", "--is-ancestor", $declaration.base, $comparisonBase)
        if ($baseAncestor.ExitCode -ne 0) { throw "Internal-refactor base is not an ancestor of the comparison base" }
    }

    $paths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $declaration.files) {
        if ($entry -isnot [System.Collections.IDictionary] -or
            ($entry.Keys | Sort-Object) -join "," -cne "after,before,path" -or
            $entry.path -isnot [string] -or $entry.path -cnotmatch '^[A-Za-z0-9_.][A-Za-z0-9_./ -]*$' -or
            @($entry.path.Split('/') | Where-Object { $_ -in @("", ".", "..", ".git") }).Count -ne 0 -or
            -not $paths.Add($entry.path)) {
            throw "Invalid or duplicate internal-refactor path"
        }
        foreach ($side in @("before", "after")) {
            if ($null -ne $entry[$side] -and ($entry[$side] -isnot [string] -or $entry[$side] -cnotmatch '^[0-9a-f]{40}$')) {
                throw "Invalid internal-refactor blob for $($entry.path)"
            }
        }
        if ($entry.before -ceq $entry.after) { throw "Internal-refactor entry has no content change: $($entry.path)" }
    }
    $declaredPaths = [string[]]@($declaration.files | ForEach-Object { $_.path })
    function Read-RefactorBlobs {
        param([string[]]$Arguments, [string]$Pattern)
        $result = Invoke-Native -FileName "git" -Arguments ($Arguments + @("--") + $declaredPaths)
        if ($result.ExitCode -ne 0) { throw "Cannot read refactor blobs" }
        $blobs = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
        foreach ($record in $result.Stdout.Split([char]0, [System.StringSplitOptions]::RemoveEmptyEntries)) {
            if ($record -cnotmatch $Pattern -or $blobs.ContainsKey($Matches[3])) {
                throw "Refactor path is not one regular file"
            }
            $blobs.Add($Matches[3], $Matches[2])
        }
        return $blobs
    }
    $treePattern = '^100(644|755) blob ([0-9a-f]{40})\t(.+)$'
    $beforeBlobs = Read-RefactorBlobs @("ls-tree", "-z", $declaration.base) $treePattern
    $comparisonBlobs = if ($null -ne $comparisonBase -and $comparisonBase -cne $declaration.base) {
        Read-RefactorBlobs @("ls-tree", "-z", $comparisonBase) $treePattern
    } else { $null }
    if ($Mode -eq "Staged") {
        $afterBlobs = Read-RefactorBlobs @("ls-files", "--stage", "-z") '^100(644|755) ([0-9a-f]{40}) 0\t(.+)$'
    } else {
        $afterBlobs = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
        $presentPaths = [string[]]@($declaredPaths | Where-Object { Test-Path -LiteralPath $_ })
        foreach ($path in $presentPaths) {
            $item = Get-Item -LiteralPath $path
            if ($item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
                throw "Refactor current path is not a regular file"
            }
        }
        if ($presentPaths.Count -gt 0) {
            $hashes = Invoke-Native -FileName "git" -Arguments (@("hash-object", "--") + $presentPaths)
            if ($hashes.ExitCode -ne 0) { throw "Cannot hash current refactor files" }
            $values = [string[]]@($hashes.Stdout.TrimEnd().Split("`n"))
            if ($values.Count -ne $presentPaths.Count) { throw "Missing refactor file hashes" }
            for ($index = 0; $index -lt $presentPaths.Count; $index++) {
                $afterBlobs.Add($presentPaths[$index], $values[$index].TrimEnd("`r"))
            }
        }
    }
    $committedBlobs = if ($null -ne $comparisonHead) {
        Read-RefactorBlobs @("ls-tree", "-z", $comparisonHead) $treePattern
    } else { $null }
    foreach ($entry in $declaration.files) {
        $before = if ($beforeBlobs.ContainsKey($entry.path)) { $beforeBlobs[$entry.path] } else { $null }
        $after = if ($afterBlobs.ContainsKey($entry.path)) { $afterBlobs[$entry.path] } else { $null }
        if ($before -cne $entry.before) { throw "Internal-refactor before blob mismatch: $($entry.path)" }
        if ($null -ne $comparisonBlobs) {
            $comparisonBefore = if ($comparisonBlobs.ContainsKey($entry.path)) { $comparisonBlobs[$entry.path] } else { $null }
            if ($comparisonBefore -cne $before) { throw "Refactor path changed at the comparison base: $($entry.path)" }
        }
        if ($after -cne $entry.after) { throw "Internal-refactor after blob mismatch: $($entry.path)" }
        if ($null -ne $committedBlobs) {
            $committed = if ($committedBlobs.ContainsKey($entry.path)) { $committedBlobs[$entry.path] } else { $null }
            if ($committed -cne $after) { throw "Refactor content differs from the compared commit: $($entry.path)" }
        }
    }
    return ,$declaredPaths
}
