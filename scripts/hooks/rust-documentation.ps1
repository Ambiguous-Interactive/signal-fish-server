Set-StrictMode -Version Latest

function Test-RustCompilerAttribute {
    param([string]$Attribute, [int]$Depth = 0, [bool]$TrustedAsyncTrait = $false, [bool]$TrustedBuiltinDerives = $false)

    if ($Depth -gt 16 -or $Attribute -cnotmatch '^\s*(?<name>[A-Za-z_][A-Za-z0-9_:]*)\s*(?<arguments>.*)$') { return $false }
    $name = $Matches.name
    $arguments = $Matches.arguments.Trim()
    if ($TrustedAsyncTrait -and $Attribute.Trim() -cmatch '^(?:::)?async_trait(?:::\s*async_trait)?\s*(?:\(\s*\?\s*Send\s*\))?$') { return $true }
    if ($name -ceq 'derive') {
        if (-not $TrustedBuiltinDerives) { return $false }
        if (-not $arguments.StartsWith('(') -or -not $arguments.EndsWith(')')) { return $false }
        foreach ($derive in $arguments.Substring(1, $arguments.Length - 2).Split(',')) {
            if ($derive.Trim() -cnotin @('', 'Clone', 'Copy', 'Debug', 'Default', 'Eq', 'Hash', 'Ord', 'PartialEq', 'PartialOrd')) { return $false }
        }
        return $true
    }
    if ($name -ceq 'cfg_attr') {
        if (-not $arguments.StartsWith('(') -or -not $arguments.EndsWith(')')) { return $false }
        $body = $arguments.Substring(1, $arguments.Length - 2)
        $parts = [System.Collections.Generic.List[string]]::new()
        $nesting = 0
        $start = 0
        for ($index = 0; $index -lt $body.Length; $index++) {
            if ($body[$index] -in @('(', '[', '{')) { $nesting++ }
            elseif ($body[$index] -in @(')', ']', '}')) { $nesting-- }
            elseif ($body[$index] -eq ',' -and $nesting -eq 0) {
                $parts.Add($body.Substring($start, $index - $start))
                $start = $index + 1
            }
        }
        $parts.Add($body.Substring($start))
        if ($parts.Count -lt 2) { return $false }
        for ($index = 1; $index -lt $parts.Count; $index++) {
            if ($parts[$index].Trim().Length -gt 0 -and -not (Test-RustCompilerAttribute -Attribute $parts[$index] -Depth ($Depth + 1) -TrustedAsyncTrait $TrustedAsyncTrait -TrustedBuiltinDerives $TrustedBuiltinDerives)) { return $false }
        }
        return $true
    }
    $name -cin @('allow', 'warn', 'deny', 'forbid', 'expect', 'cfg', 'doc', 'inline', 'cold', 'track_caller', 'must_use', 'deprecated', 'repr', 'non_exhaustive', 'no_mangle', 'export_name', 'link_name', 'link_section', 'used', 'global_allocator', 'panic_handler', 'alloc_error_handler', 'test', 'ignore', 'should_panic', 'path', 'macro_use', 'macro_export', 'automatically_derived', 'no_std', 'no_main', 'recursion_limit', 'type_length_limit')
}

function Get-RustAttributedItemEnd {
    param([string]$Code, [int]$Start)

    $signature = $Code.Substring($Start)
    $item = [regex]::Match($signature, '^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:(?:unsafe|async|default|auto)\s+)*(?<kind>fn|struct|enum|union|trait|impl|mod|extern|const|static|type|use|let|macro_rules|macro)\b')
    if (-not $item.Success) {
        $scopeDepth = 0
        foreach ($punctuation in [regex]::Matches($signature, '[()\[\]{}]')) {
            if ($punctuation.Value -cin @('(', '[', '{')) { $scopeDepth++ }
            elseif ($scopeDepth -eq 0) { return $Start + $punctuation.Index + 1 }
            else { $scopeDepth-- }
        }
        return $Code.Length
    }
    $kind = $item.Groups['kind'].Value
    $semicolonItem = $kind -cin @('const', 'static', 'type', 'use', 'let')
    if ($kind -ceq 'const' -and $signature -cmatch '^\s*(?:pub(?:\s*\([^)]*\))?\s+)?const\s+(?:(?:unsafe|async|extern)\s+)*fn\b') { $semicolonItem = $false }
    $signatureDepth = 0
    $angleDepth = 0
    $bodyDepth = 0
    foreach ($punctuation in [regex]::Matches($signature, '->|[()\[\]{}<>;]')) {
        $value = $punctuation.Value
        if ($bodyDepth -gt 0) {
            if ($value -cin @('(', '[', '{')) { $bodyDepth++ }
            elseif ($value -cin @(')', ']', '}')) {
                $bodyDepth--
                if ($bodyDepth -eq 0) { return $Start + $punctuation.Index + $punctuation.Length }
            }
            continue
        }
        if ($value -ceq '->') { continue }
        if ($value -cin @('(', '[')) { $signatureDepth++; continue }
        if ($value -cin @(')', ']')) {
            $signatureDepth--
            if ($signatureDepth -lt 0) { return $Code.Length }
            continue
        }
        if ($value -ceq '{') {
            if (-not $semicolonItem -and $signatureDepth -eq 0 -and $angleDepth -eq 0) { $bodyDepth = 1 }
            else { $signatureDepth++ }
            continue
        }
        if ($value -ceq '}') {
            $signatureDepth--
            if ($signatureDepth -lt 0) { return $Code.Length }
            continue
        }
        if (-not $semicolonItem -and $signatureDepth -eq 0) {
            if ($value -ceq '<') { $angleDepth++ }
            elseif ($value -ceq '>') { $angleDepth = [System.Math]::Max(0, $angleDepth - 1) }
        }
        if ($value -ceq ';' -and $signatureDepth -eq 0 -and $angleDepth -eq 0) { return $Start + $punctuation.Index + 1 }
    }
    $Code.Length
}

function Remove-RustDocumentationComments {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$Content,
        [int]$ChangedStart = 0,
        [int]$ChangedEnd = [int]::MaxValue
    )

    $tokens = [regex]::new(@'
(?x)
(?<literal>(?:br|cr|r)(?<hash>\#{0,255})"(?s:.*?)"\k<hash>
| "(?:\\(?s:.)|[^"\\])*"
| '(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|(?s:.))|[^'\\\r\n])')
| (?<comment>/\*(?>[^*/]+|/(?!\*)|\*(?!/)|(?<depth>/\*)|(?<-depth>\*/))*(?(depth)(?!))\*/
| //[^\r\n]*)
| (?<invalid>(?:br|cr|r)\#*"|/\*|")
| (?:(?!(?:br|cr|r)\#*")[^/"'])+
| .
'@, [System.Text.RegularExpressions.RegexOptions]::Singleline,
        [System.TimeSpan]::FromMilliseconds(100))
    $mask = [System.Text.StringBuilder]::new($Content.Length)
    $documentationTokens = [System.Collections.Generic.List[System.Text.RegularExpressions.Match]]::new()
    foreach ($token in $tokens.Matches($Content)) {
        if ($token.Groups['invalid'].Success) { throw 'Incomplete Rust literal or comment' }
        if ($token.Groups['literal'].Success -or $token.Groups['comment'].Success) {
            if ($token.Value.Contains("`n") -or $token.Value.Contains("`r")) {
                [void]$mask.Append([regex]::Replace($token.Value, '[^\r\n]', ' '))
            } else {
                [void]$mask.Append([char]' ', $token.Length)
            }
        } else {
            [void]$mask.Append($token.Value)
        }
        if (-not $token.Groups['comment'].Success) { continue }
        if ($token.Index -gt $ChangedEnd -or $token.Index + $token.Length -lt $ChangedStart) { continue }
        $documentation = $token.Value.StartsWith('///', [System.StringComparison]::Ordinal) -and -not $token.Value.StartsWith('////', [System.StringComparison]::Ordinal)
        $documentation = $documentation -or $token.Value.StartsWith('//!', [System.StringComparison]::Ordinal)
        $documentation = $documentation -or ($token.Value -cne '/**/' -and $token.Value.StartsWith('/**', [System.StringComparison]::Ordinal) -and -not $token.Value.StartsWith('/***', [System.StringComparison]::Ordinal))
        $documentation = $documentation -or $token.Value.StartsWith('/*!', [System.StringComparison]::Ordinal)
        if ($documentation) { $documentationTokens.Add($token) }
    }
    if ($documentationTokens.Count -eq 0) { return $Content }
    $code = $mask.ToString()
    $options = [System.Text.RegularExpressions.RegexOptions]::Singleline
    $timeout = [System.TimeSpan]::FromMilliseconds(100)
    $macroPattern = '!\s*(?:[^\s()\[\]{};!,]+\s*)?[([{](?>[^()\[\]{}]+|(?<nested>[([{])|(?<-nested>[)\]}]))*(?(nested)(?!))[)\]}]'
    $macroSpans = [regex]::new($macroPattern, $options, $timeout).Matches($code)
    if ([regex]::IsMatch($code, '\bmacro\s+\S', $options, $timeout)) { throw 'Unclassified macro definition' }
    $attributePattern = '#\s*!?\s*\[(?<body>(?>[^\[\]]+|(?<nested>\[)|(?<-nested>\]))*)(?(nested)(?!))\]'
    $attributes = [regex]::new($attributePattern, $options, $timeout).Matches($code)
    $asyncTraitImport = '(?m)^use\s+(?:::)?async_trait\s*::\s*async_trait\s*;'
    $asyncTraitAttribute = '#\s*\[\s*(?:::)?async_trait(?:::\s*async_trait)?\s*(?:\(\s*\?\s*Send\s*\))?\s*\]'
    $withoutAsyncTrait = [regex]::Replace([regex]::Replace($code, $asyncTraitImport, ''), $asyncTraitAttribute, '')
    $trustedAsyncTrait = -not [regex]::IsMatch($withoutAsyncTrait, '\basync_trait\b')
    if ($trustedAsyncTrait) {
        $shortAsyncTrait = [regex]::IsMatch($code, '#\s*\[\s*async_trait\s*(?:\(|\])')
        if ($shortAsyncTrait -and [regex]::Matches($code, $asyncTraitImport).Count -ne 1) { $trustedAsyncTrait = $false }
    }
    $testModulePattern = '(?m)^#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*(?:#\[[^\]]*\]\s*)*mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{(?>[^{}]+|(?<nested>\{)|(?<-nested>\}))*(?(nested)(?!))\}\s*\z'
    $testModule = [regex]::new($testModulePattern, $options, $timeout).Match($code)
    $productionCode = if ($testModule.Success) { $code.Substring(0, $testModule.Index) } else { $code }
    $deriveNames = '(?:Clone|Copy|Debug|Default|Eq|Hash|Ord|PartialEq|PartialOrd)'
    $trustedBuiltinDerives = $true
    foreach ($import in [regex]::Matches($productionCode, '(?s)\buse\b[^;]*;')) {
        if ($import.Value.Contains('*') -or [regex]::IsMatch($import.Value, "\b$deriveNames\b")) { $trustedBuiltinDerives = $false; break }
    }
    if ([regex]::IsMatch($productionCode, "\b(?:mod|macro_rules\s*!)\s+(?:r#)?$deriveNames\b|\bextern\s+crate\b[^;]*\bas\s+$deriveNames\b")) { $trustedBuiltinDerives = $false }
    $macroBracePosition = 0
    $macroBraceDepth = 0
    $macroBracePositions = [regex]::Matches($productionCode, '[{}]')
    foreach ($macro in $macroSpans) {
        if ($macro.Index -ge $productionCode.Length) { break }
        while ($macroBracePosition -lt $macroBracePositions.Count -and $macroBracePositions[$macroBracePosition].Index -lt $macro.Index) {
            if ($macroBracePositions[$macroBracePosition].Value -ceq '{') { $macroBraceDepth++ } else { $macroBraceDepth-- }
            $macroBracePosition++
        }
        if ($macroBraceDepth -eq 0) { $trustedBuiltinDerives = $false; $trustedAsyncTrait = $false; break }
    }
    $unsafeAttributeSpans = [System.Collections.Generic.List[object]]::new()
    $bracePositions = [regex]::Matches($code, '[{}]')
    $bracePosition = 0
    $braceDepth = 0
    for ($attributeIndex = 0; $attributeIndex -lt $attributes.Count; $attributeIndex++) {
        $attribute = $attributes[$attributeIndex]
        if ($testModule.Success -and $attribute.Index -ge $testModule.Index) { break }
        if ($unsafeAttributeSpans.Count -gt 0 -and $unsafeAttributeSpans[$unsafeAttributeSpans.Count - 1].End -gt $attribute.Index) { continue }
        while ($bracePosition -lt $bracePositions.Count -and $bracePositions[$bracePosition].Index -lt $attribute.Index) {
            if ($bracePositions[$bracePosition].Value -ceq '{') { $braceDepth++ } else { $braceDepth-- }
            $bracePosition++
        }
        if (Test-RustCompilerAttribute -Attribute $attribute.Groups['body'].Value -TrustedAsyncTrait ($trustedAsyncTrait -and $braceDepth -eq 0) -TrustedBuiltinDerives ($trustedBuiltinDerives -and $braceDepth -eq 0)) { continue }
        if ($attribute.Value -cmatch '^#\s*!') { throw 'Unclassified inner Rust attribute' }
        $start = $attribute.Index
        for ($previous = $attributeIndex - 1; $previous -ge 0; $previous--) {
            $previousEnd = $attributes[$previous].Index + $attributes[$previous].Length
            if ($code.Substring($previousEnd, $start - $previousEnd).Trim().Length -ne 0) { break }
            $start = $attributes[$previous].Index
        }
        while ($start -gt 0 -and [char]::IsWhiteSpace($code[$start - 1])) { $start-- }
        $end = $attribute.Index + $attribute.Length
        for ($next = $attributeIndex + 1; $next -lt $attributes.Count; $next++) {
            if ($code.Substring($end, $attributes[$next].Index - $end).Trim().Length -ne 0) { break }
            $end = $attributes[$next].Index + $attributes[$next].Length
        }
        $end = Get-RustAttributedItemEnd -Code $code -Start $end
        $unsafeAttributeSpans.Add(@{ Start = $start; End = $end })
    }
    foreach ($macro in $macroSpans) { $unsafeAttributeSpans.Add(@{ Start = $macro.Index; End = $macro.Index + $macro.Length }) }
    $unsafeSpans = [System.Collections.Generic.List[object]]::new()
    foreach ($span in @($unsafeAttributeSpans | Sort-Object -Property Start, End)) {
        if ($unsafeSpans.Count -gt 0 -and $span.Start -le $unsafeSpans[$unsafeSpans.Count - 1].End) {
            $previous = $unsafeSpans[$unsafeSpans.Count - 1]
            $previous.End = [System.Math]::Max($previous.End, $span.End)
        } else { $unsafeSpans.Add($span) }
    }
    $unsafePosition = 0
    $result = [System.Text.StringBuilder]::new($Content.Length)
    $retained = 0
    foreach ($token in $documentationTokens) {
        while ($unsafePosition -lt $unsafeSpans.Count -and $unsafeSpans[$unsafePosition].End -le $token.Index) { $unsafePosition++ }
        if ($unsafePosition -lt $unsafeSpans.Count -and $unsafeSpans[$unsafePosition].Start -le $token.Index) { continue }
        $start = $Content.LastIndexOf("`n", [System.Math]::Max(0, $token.Index - 1)) + 1
        $end = $token.Index + $token.Length
        $newline = $Content.IndexOf("`n", $end)
        $lineEnd = if ($newline -lt 0) { $Content.Length } else { $newline }
        if ($Content.Substring($start, $token.Index - $start).Trim().Length -ne 0 -or $Content.Substring($end, $lineEnd - $end).Trim().Length -ne 0) { continue }
        [void]$result.Append($Content, $retained, $start - $retained)
        $retained = if ($newline -lt 0) { $lineEnd } else { $newline + 1 }
    }
    [void]$result.Append($Content, $retained, $Content.Length - $retained)
    $result.ToString()
}

function Test-RustDocumentationOnlyChange {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$OldContent,
        [Parameter(Mandatory = $true)][AllowEmptyString()][string]$NewContent
    )
    if ([string]::Equals($OldContent, $NewContent, [System.StringComparison]::Ordinal)) { return $false }
    $OldContent = $OldContent.Replace("`r`n", "`n")
    $NewContent = $NewContent.Replace("`r`n", "`n")
    $maximum = [System.Math]::Min($OldContent.Length, $NewContent.Length)
    $lower = 0
    $upper = $maximum
    while ($lower -lt $upper) {
        $middle = $lower + [int][System.Math]::Ceiling(($upper - $lower) / 2)
        if ([string]::CompareOrdinal($OldContent, 0, $NewContent, 0, $middle) -eq 0) { $lower = $middle }
        else { $upper = $middle - 1 }
    }
    $changedStart = $lower
    $lower = 0
    $upper = $maximum - $changedStart
    while ($lower -lt $upper) {
        $middle = $lower + [int][System.Math]::Ceiling(($upper - $lower) / 2)
        if ([string]::CompareOrdinal($OldContent, $OldContent.Length - $middle, $NewContent, $NewContent.Length - $middle, $middle) -eq 0) { $lower = $middle }
        else { $upper = $middle - 1 }
    }
    try {
        if ($OldContent.Length - $lower -eq $changedStart) {
            $newSource = Remove-RustDocumentationComments -Content $NewContent -ChangedStart $changedStart -ChangedEnd ($NewContent.Length - $lower)
            if ([string]::Equals($OldContent, $newSource, [System.StringComparison]::Ordinal)) { return $true }
            $oldSource = Remove-RustDocumentationComments -Content $OldContent -ChangedStart $changedStart -ChangedEnd ($OldContent.Length - $lower)
        } else {
            $oldSource = Remove-RustDocumentationComments -Content $OldContent -ChangedStart $changedStart -ChangedEnd ($OldContent.Length - $lower)
            if ([string]::Equals($oldSource, $NewContent, [System.StringComparison]::Ordinal)) { return $true }
            $newSource = Remove-RustDocumentationComments -Content $NewContent -ChangedStart $changedStart -ChangedEnd ($NewContent.Length - $lower)
        }
        [string]::Equals($oldSource, $newSource, [System.StringComparison]::Ordinal)
    } catch {
        $false
    }
}
