#requires -Version 7.0

# Rust panic policy loaded lazily by pre-commit.ps1.

function Add-LineRange {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.Collections.Generic.HashSet[int]]$LineSet,
        [Parameter(Mandatory = $true)][int]$StartLine,
        [Parameter(Mandatory = $true)][int]$EndLine
    )

    for ($lineNumber = $StartLine; $lineNumber -le $EndLine; $lineNumber++) {
        [void]$LineSet.Add($lineNumber)
    }
}

function Test-RustCfgTestAttributeLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    if (-not $Line.TrimStart().StartsWith("#[")) {
        return $false
    }

    if ($Line -notmatch "#\[\s*cfg\s*\((?<expr>[^\]]*)\)\s*\]") {
        return $false
    }

    Test-RustCfgExpressionRequiresTest -Expression $Matches["expr"]
}

function Test-RustDirectTestAttributeLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    if (-not $Line.TrimStart().StartsWith("#[")) {
        return $false
    }

    $Line -match "#\[\s*(test|tokio::test|async_std::test|rstest)(\(|\])"
}

function Split-RustCfgArguments {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Expression)

    $arguments = [System.Collections.Generic.List[string]]::new()
    $start = 0
    $depth = 0
    $inString = $false
    $escape = $false

    for ($index = 0; $index -lt $Expression.Length; $index++) {
        $char = $Expression[$index]
        if ($inString) {
            if ($escape) {
                $escape = $false
            } elseif ($char -eq [char]"\") {
                $escape = $true
            } elseif ($char -eq [char]"""") {
                $inString = $false
            }
            continue
        }

        if ($char -eq [char]"""") {
            $inString = $true
            $escape = $false
        } elseif ($char -eq [char]"(") {
            $depth++
        } elseif ($char -eq [char]")") {
            if ($depth -gt 0) {
                $depth--
            }
        } elseif ($char -eq [char]"," -and $depth -eq 0) {
            [void]$arguments.Add($Expression.Substring($start, $index - $start).Trim())
            $start = $index + 1
        }
    }

    $tail = $Expression.Substring($start).Trim()
    if (-not [string]::IsNullOrWhiteSpace($tail)) {
        [void]$arguments.Add($tail)
    }

    $arguments.ToArray()
}

function Test-RustCfgExpressionRequiresTest {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Expression)

    $expr = $Expression.Trim()
    if ($expr -eq "test") {
        return $true
    }
    if ($expr.StartsWith("not(")) {
        return $false
    }
    if ($expr.StartsWith("all(") -and $expr.EndsWith(")")) {
        $inner = $expr.Substring(4, $expr.Length - 5)
        foreach ($argument in (Split-RustCfgArguments -Expression $inner)) {
            if (Test-RustCfgExpressionRequiresTest -Expression $argument) {
                return $true
            }
        }
        return $false
    }
    if ($expr.StartsWith("any(") -and $expr.EndsWith(")")) {
        $arguments = @(Split-RustCfgArguments -Expression $expr.Substring(4, $expr.Length - 5))
        if ($arguments.Count -eq 0) {
            return $false
        }
        foreach ($argument in $arguments) {
            if (-not (Test-RustCfgExpressionRequiresTest -Expression $argument)) {
                return $false
            }
        }
        return $true
    }

    $false
}

function Get-RustAttributedItemLine {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory = $true)][int]$StartIndex
    )

    for ($index = $StartIndex; $index -lt $Lines.Count; $index++) {
        $remaining = $Lines[$index].Trim()
        if ([string]::IsNullOrWhiteSpace($remaining)) {
            continue
        }

        while ($remaining.StartsWith("#[")) {
            $attributeEnd = $remaining.IndexOf("]")
            if ($attributeEnd -lt 0) {
                break
            }
            $remaining = $remaining.Substring($attributeEnd + 1).TrimStart()
        }

        if ([string]::IsNullOrWhiteSpace($remaining)) {
            continue
        }

        return $index
    }

    -1
}

function Get-RustItemEndLine {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyString()][string[]]$Lines,
        [Parameter(Mandatory = $true)][int]$StartIndex
    )

    $depth = 0
    $opened = $false
    $blockCommentDepth = 0
    $inString = $false
    $inChar = $false
    $escape = $false
    $rawTerminator = ""

    for ($index = $StartIndex; $index -lt $Lines.Count; $index++) {
        $line = $Lines[$index]
        $charIndex = 0
        while ($charIndex -lt $line.Length) {
            if ($blockCommentDepth -gt 0) {
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                    $blockCommentDepth++
                    $charIndex += 2
                    continue
                }
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"*" -and $line[$charIndex + 1] -eq [char]"/") {
                    $blockCommentDepth--
                    $charIndex += 2
                    continue
                }
                $charIndex++
                continue
            }

            if (-not [string]::IsNullOrEmpty($rawTerminator)) {
                if ($line.Substring($charIndex).StartsWith($rawTerminator)) {
                    $charIndex += $rawTerminator.Length
                    $rawTerminator = ""
                    continue
                }
                $charIndex++
                continue
            }

            if ($inString) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"""") {
                    $inString = $false
                }
                $charIndex++
                continue
            }

            if ($inChar) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"'") {
                    $inChar = $false
                }
                $charIndex++
                continue
            }

            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"/") {
                break
            }
            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                $blockCommentDepth = 1
                $charIndex += 2
                continue
            }

            $rawStart = -1
            if ($line[$charIndex] -eq [char]"r") {
                $rawStart = $charIndex
            } elseif ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"r") {
                $rawStart = $charIndex + 1
            }
            if ($rawStart -ge 0) {
                $probe = $rawStart + 1
                while ($probe -lt $line.Length -and $line[$probe] -eq [char]"#") {
                    $probe++
                }
                if ($probe -lt $line.Length -and $line[$probe] -eq [char]"""") {
                    $hashes = $line.Substring($rawStart + 1, $probe - $rawStart - 1)
                    $rawTerminator = """" + $hashes
                    $charIndex = $probe + 1
                    continue
                }
            }

            if ($line[$charIndex] -eq [char]"""" -or (
                    $charIndex + 1 -lt $line.Length -and
                    $line[$charIndex] -eq [char]"b" -and
                    $line[$charIndex + 1] -eq [char]""""
                )) {
                $inString = $true
                $escape = $false
                $charIndex += if ($line[$charIndex] -eq [char]"b") { 2 } else { 1 }
                continue
            }

            if ($line[$charIndex] -eq [char]"'") {
                $nextIndex = $charIndex + 1
                if ($nextIndex -lt $line.Length -and ([string]$line[$nextIndex]) -match "[A-Za-z_]") {
                    $afterIdentifier = $nextIndex + 1
                    while ($afterIdentifier -lt $line.Length -and ([string]$line[$afterIdentifier]) -match "[A-Za-z0-9_]") {
                        $afterIdentifier++
                    }
                    if ($afterIdentifier -lt $line.Length -and $line[$afterIdentifier] -eq [char]"'") {
                        $charIndex = $afterIdentifier + 1
                        continue
                    }

                    $charIndex++
                    continue
                }

                $probe = $nextIndex
                $escapedChar = $false
                $charLiteralEnd = -1
                while ($probe -lt $line.Length) {
                    if ($escapedChar) {
                        $escapedChar = $false
                    } elseif ($line[$probe] -eq [char]"\") {
                        $escapedChar = $true
                    } elseif ($line[$probe] -eq [char]"'") {
                        $charLiteralEnd = $probe
                        break
                    }
                    $probe++
                }
                if ($charLiteralEnd -ge 0) {
                    $charIndex = $charLiteralEnd + 1
                    continue
                }

                $charIndex++
                continue
            }

            if ($line[$charIndex] -eq [char]"{") {
                $opened = $true
                $depth++
            } elseif ($line[$charIndex] -eq [char]"}") {
                $depth--
            } elseif ($line[$charIndex] -eq [char]";" -and -not $opened) {
                return $index + 1
            }

            $charIndex++
        }

        if ($opened -and $depth -le 0) {
            return $index + 1
        }
    }

    $Lines.Count
}

function Get-RustTestLineSet {
    param(
        [AllowNull()][string]$Content,
        [int]$MaxLine = 0
    )

    $testLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content)) {
        Write-Output -NoEnumerate $testLines
        return
    }

    $lines = [string[]]($Content -split "`r?`n")
    $lineLimit = $lines.Count
    if ($MaxLine -gt 0) {
        $lineLimit = [System.Math]::Min($MaxLine, $lines.Count)
    }

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $trimmed = $lines[$index].Trim()

        if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
            $itemIndex = Get-RustAttributedItemLine -Lines $lines -StartIndex $index
            if ($itemIndex -lt 0) {
                continue
            }

            $endLine = Get-RustItemEndLine -Lines $lines -StartIndex $itemIndex
            Add-LineRange -LineSet $testLines -StartLine ($index + 1) -EndLine ([System.Math]::Min($endLine, $lineLimit))
            $index = $endLine - 1
        }
    }

    Write-Output -NoEnumerate $testLines
}

function Test-RustPanicMacroLine {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $macroNames = [System.Collections.Generic.HashSet[string]]::new(
        [string[]]@("panic", "todo", "unimplemented", "unreachable"),
        [System.StringComparer]::Ordinal
    )

    $charIndex = 0
    $inString = $false
    $inChar = $false
    $escape = $false
    $rawTerminator = ""
    while ($charIndex -lt $Line.Length) {
        if (-not [string]::IsNullOrEmpty($rawTerminator)) {
            if ($Line.Substring($charIndex).StartsWith($rawTerminator, [System.StringComparison]::Ordinal)) {
                $charIndex += $rawTerminator.Length
                $rawTerminator = ""
                continue
            }
            $charIndex++
            continue
        }

        if ($inString) {
            if ($escape) {
                $escape = $false
            } elseif ($Line[$charIndex] -eq [char]"\") {
                $escape = $true
            } elseif ($Line[$charIndex] -eq [char]"""") {
                $inString = $false
            }
            $charIndex++
            continue
        }

        if ($inChar) {
            if ($escape) {
                $escape = $false
            } elseif ($Line[$charIndex] -eq [char]"\") {
                $escape = $true
            } elseif ($Line[$charIndex] -eq [char]"'") {
                $inChar = $false
            }
            $charIndex++
            continue
        }

        if ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"/" -and $Line[$charIndex + 1] -eq [char]"/") {
            return $false
        }
        if ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"/" -and $Line[$charIndex + 1] -eq [char]"*") {
            $end = $Line.IndexOf("*/", $charIndex + 2, [System.StringComparison]::Ordinal)
            if ($end -lt 0) {
                return $false
            }
            $charIndex = $end + 2
            continue
        }

        $rawStart = -1
        if ($Line[$charIndex] -eq [char]"r") {
            $rawStart = $charIndex
        } elseif ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"b" -and $Line[$charIndex + 1] -eq [char]"r") {
            $rawStart = $charIndex + 1
        }
        if ($rawStart -ge 0) {
            $probe = $rawStart + 1
            while ($probe -lt $Line.Length -and $Line[$probe] -eq [char]"#") {
                $probe++
            }
            if ($probe -lt $Line.Length -and $Line[$probe] -eq [char]"""") {
                $hashes = $Line.Substring($rawStart + 1, $probe - $rawStart - 1)
                $rawTerminator = """" + $hashes
                $charIndex = $probe + 1
                continue
            }
        }

        if (
            $Line[$charIndex] -eq [char]"""" -or
            ($charIndex + 1 -lt $Line.Length -and $Line[$charIndex] -eq [char]"b" -and $Line[$charIndex + 1] -eq [char]"""")
        ) {
            $inString = $true
            $escape = $false
            $charIndex += if ($Line[$charIndex] -eq [char]"b") { 2 } else { 1 }
            continue
        }

        if ($Line[$charIndex] -eq [char]"'") {
            $nextIndex = $charIndex + 1
            if ($nextIndex -lt $Line.Length -and (Test-RustIdentifierStartChar -Char $Line[$nextIndex])) {
                $afterIdentifier = $nextIndex + 1
                while ($afterIdentifier -lt $Line.Length -and (Test-RustIdentifierChar -Char $Line[$afterIdentifier])) {
                    $afterIdentifier++
                }
                if ($afterIdentifier -lt $Line.Length -and $Line[$afterIdentifier] -ne [char]"'") {
                    $charIndex = $afterIdentifier
                    continue
                }
            }

            $inChar = $true
            $escape = $false
            $charIndex++
            continue
        }

        if (Test-RustIdentifierStartChar -Char $Line[$charIndex]) {
            $start = $charIndex
            $charIndex++
            while ($charIndex -lt $Line.Length -and (Test-RustIdentifierChar -Char $Line[$charIndex])) {
                $charIndex++
            }

            $identifier = $Line.Substring($start, $charIndex - $start)
            if ($macroNames.Contains($identifier)) {
                $probe = $charIndex
                $canProbe = $true
                foreach ($expected in @("!", "delimiter")) {
                    while ($probe -lt $Line.Length) {
                        if ([char]::IsWhiteSpace($Line[$probe])) {
                            $probe++
                            continue
                        }
                        if ($probe + 1 -lt $Line.Length -and $Line[$probe] -eq [char]"/" -and $Line[$probe + 1] -eq [char]"*") {
                            $end = $Line.IndexOf("*/", $probe + 2, [System.StringComparison]::Ordinal)
                            if ($end -lt 0) {
                                $canProbe = $false
                                break
                            }
                            $probe = $end + 2
                            continue
                        }
                        break
                    }
                    if (-not $canProbe -or $probe -ge $Line.Length) {
                        $canProbe = $false
                        break
                    }
                    if ($expected -eq "delimiter") {
                        if ($Line[$probe] -notin @([char]"(", [char]"[", [char]"{")) {
                            $canProbe = $false
                            break
                        }
                    } elseif ($Line[$probe] -ne [char]$expected) {
                        $canProbe = $false
                        break
                    }
                    $probe++
                }
                if ($canProbe) {
                    return $true
                }
            }
            continue
        }

        $charIndex++
    }

    $false
}

function Test-RustIdentifierStartChar {
    param([Parameter(Mandatory = $true)][char]$Char)

    $code = [int]$Char
    ($code -ge 65 -and $code -le 90) -or
    ($code -ge 97 -and $code -le 122) -or
    $Char -eq [char]"_"
}

function Test-RustIdentifierChar {
    param([Parameter(Mandatory = $true)][char]$Char)

    $code = [int]$Char
    ($code -ge 65 -and $code -le 90) -or
    ($code -ge 97 -and $code -le 122) -or
    ($code -ge 48 -and $code -le 57) -or
    $Char -eq [char]"_"
}

function Get-RustExecutablePanicMacroLineSet {
    param(
        [AllowNull()][string]$Content,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][int[]]$CandidateLines
    )

    $macroLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content) -or $CandidateLines.Count -eq 0) {
        Write-Output -NoEnumerate $macroLines
        return
    }

    $candidateLineSet = [System.Collections.Generic.HashSet[int]]::new()
    $maxCandidateLine = 0
    foreach ($lineNumber in $CandidateLines) {
        [void]$candidateLineSet.Add($lineNumber)
        if ($lineNumber -gt $maxCandidateLine) {
            $maxCandidateLine = $lineNumber
        }
    }

    $lines = [string[]](($Content -replace "`r`n", "`n" -replace "`r", "`n") -split "`n")
    $lineLimit = [System.Math]::Min($maxCandidateLine, $lines.Count)
    $blockCommentDepth = 0
    $rawTerminator = ""
    $inString = $false
    $inChar = $false
    $escape = $false

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $lineNumber = $index + 1
        $line = $lines[$index]
        $lineStartsInsideSuppressedContext =
            $blockCommentDepth -gt 0 -or
            -not [string]::IsNullOrEmpty($rawTerminator) -or
            $inString -or
            $inChar

        if (
            $candidateLineSet.Contains($lineNumber) -and
            -not $lineStartsInsideSuppressedContext -and
            (Test-RustPanicMacroLine -Line $line)
        ) {
            [void]$macroLines.Add($lineNumber)
        }

        $charIndex = 0
        while ($charIndex -lt $line.Length) {
            if ($blockCommentDepth -gt 0) {
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                    $blockCommentDepth++
                    $charIndex += 2
                    continue
                }
                if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"*" -and $line[$charIndex + 1] -eq [char]"/") {
                    $blockCommentDepth--
                    $charIndex += 2
                    continue
                }
                $charIndex++
                continue
            }

            if (-not [string]::IsNullOrEmpty($rawTerminator)) {
                if ($line.Substring($charIndex).StartsWith($rawTerminator, [System.StringComparison]::Ordinal)) {
                    $charIndex += $rawTerminator.Length
                    $rawTerminator = ""
                    continue
                }
                $charIndex++
                continue
            }

            if ($inString) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"""") {
                    $inString = $false
                }
                $charIndex++
                continue
            }

            if ($inChar) {
                if ($escape) {
                    $escape = $false
                } elseif ($line[$charIndex] -eq [char]"\") {
                    $escape = $true
                } elseif ($line[$charIndex] -eq [char]"'") {
                    $inChar = $false
                }
                $charIndex++
                continue
            }

            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"/") {
                break
            }
            if ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"/" -and $line[$charIndex + 1] -eq [char]"*") {
                $blockCommentDepth = 1
                $charIndex += 2
                continue
            }

            $rawStart = -1
            if ($line[$charIndex] -eq [char]"r") {
                $rawStart = $charIndex
            } elseif ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"r") {
                $rawStart = $charIndex + 1
            }
            if ($rawStart -ge 0) {
                $probe = $rawStart + 1
                while ($probe -lt $line.Length -and $line[$probe] -eq [char]"#") {
                    $probe++
                }
                if ($probe -lt $line.Length -and $line[$probe] -eq [char]"""") {
                    $hashes = $line.Substring($rawStart + 1, $probe - $rawStart - 1)
                    $rawTerminator = """" + $hashes
                    $charIndex = $probe + 1
                    continue
                }
            }

            if (
                $line[$charIndex] -eq [char]"""" -or
                ($charIndex + 1 -lt $line.Length -and $line[$charIndex] -eq [char]"b" -and $line[$charIndex + 1] -eq [char]"""")
            ) {
                $inString = $true
                $escape = $false
                $charIndex += if ($line[$charIndex] -eq [char]"b") { 2 } else { 1 }
                continue
            }

            if ($line[$charIndex] -eq [char]"'") {
                $inChar = $true
                $escape = $false
                $charIndex++
                continue
            }

            $charIndex++
        }
    }

    Write-Output -NoEnumerate $macroLines
}

function Get-RustLineBraceDelta {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Line)

    $withoutLineComment = $Line.Split([string[]]@("//"), 2, [System.StringSplitOptions]::None)[0]
    $delta = 0
    foreach ($char in $withoutLineComment.ToCharArray()) {
        if ($char -eq [char]"{") {
            $delta++
        } elseif ($char -eq [char]"}") {
            $delta--
        }
    }

    $delta
}

function Get-RustCandidateTestLineSet {
    param(
        [AllowNull()][string]$Content,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][int[]]$CandidateLines
    )

    $testCandidateLines = [System.Collections.Generic.HashSet[int]]::new()
    if ([string]::IsNullOrEmpty($Content) -or $CandidateLines.Count -eq 0) {
        Write-Output -NoEnumerate $testCandidateLines
        return
    }

    $candidateLineSet = [System.Collections.Generic.HashSet[int]]::new()
    $maxCandidateLine = 0
    foreach ($lineNumber in $CandidateLines) {
        [void]$candidateLineSet.Add($lineNumber)
        if ($lineNumber -gt $maxCandidateLine) {
            $maxCandidateLine = $lineNumber
        }
    }
    $lines = [string[]](($Content -replace "`r`n", "`n" -replace "`r", "`n") -split "`n")
    $lineLimit = [System.Math]::Min($maxCandidateLine, $lines.Count)
    $depth = 0
    $pendingTestAttribute = $false
    $pendingTestItemDepth = $null
    $testRegionDepths = [System.Collections.Generic.List[int]]::new()

    for ($index = 0; $index -lt $lineLimit; $index++) {
        $line = $lines[$index]
        $trimmed = $line.Trim()

        for ($regionIndex = $testRegionDepths.Count - 1; $regionIndex -ge 0; $regionIndex--) {
            if ($depth -lt $testRegionDepths[$regionIndex]) {
                $testRegionDepths.RemoveAt($regionIndex)
            }
        }

        $lineNumber = $index + 1
        if ($candidateLineSet.Contains($lineNumber)) {
            foreach ($regionDepth in $testRegionDepths) {
                if ($depth -ge $regionDepth) {
                    [void]$testCandidateLines.Add($lineNumber)
                    break
                }
            }
        }

        if ($trimmed.StartsWith("#[")) {
            if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
                $pendingTestAttribute = $true
            }
            continue
        }
        if ([string]::IsNullOrWhiteSpace($trimmed)) {
            continue
        }

        if ($pendingTestAttribute) {
            $pendingTestItemDepth = $depth
            $pendingTestAttribute = $false
        }

        $newDepth = $depth + (Get-RustLineBraceDelta -Line $line)
        if ($null -ne $pendingTestItemDepth) {
            if ($newDepth -gt $pendingTestItemDepth) {
                [void]$testRegionDepths.Add($pendingTestItemDepth + 1)
                $pendingTestItemDepth = $null
            } elseif ($trimmed.Contains(";")) {
                $pendingTestItemDepth = $null
            }
        }

        $depth = [System.Math]::Max(0, $newDepth)
        for ($regionIndex = $testRegionDepths.Count - 1; $regionIndex -ge 0; $regionIndex--) {
            if ($depth -lt $testRegionDepths[$regionIndex]) {
                $testRegionDepths.RemoveAt($regionIndex)
            }
        }
    }

    Write-Output -NoEnumerate $testCandidateLines
}

function Get-WorktreeRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs
    )

    $candidates = [System.Collections.Generic.List[object]]::new()
    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $trackedFiles = [string[]]@(Get-IndexFiles -Pathspecs $Pathspecs)
    $tracked = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($file in $trackedFiles) {
        [void]$tracked.Add($file)
    }

    if ($trackedFiles.Count -gt 0) {
        $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
        $result = Invoke-Native -FileName "git" -Arguments (@("grep", "-n", "-E", $grepPattern, "--") + $trackedFiles)
        if ($result.ExitCode -ne 0 -and $result.ExitCode -ne 1) {
            throw "git grep failed:`n$($result.Output)"
        }
        if ($result.ExitCode -eq 0) {
            foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
                $line = $rawLine.TrimEnd("`r")
                if ($line -notmatch "^(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
                    throw "Unable to parse git grep output line: $line"
                }

                $text = $Matches["text"]
                if (Test-RustPanicMacroLine -Line $text) {
                    [void]$candidates.Add([pscustomobject]@{
                            File = $Matches["file"]
                            Line = [int]$Matches["line"]
                            Text = $text.TrimStart()
                        })
                }
            }
        }
    }

    foreach ($file in $Pathspecs) {
        if ($tracked.Contains($file)) {
            continue
        }

        $content = Get-PolicyText -Path $file
        if ($null -eq $content) {
            continue
        }

        $lineNumber = 0
        foreach ($line in $content -split "`r?`n") {
            $lineNumber++
            if (Test-RustPanicMacroLine -Line $line) {
                [void]$candidates.Add([pscustomobject]@{
                        File = $file
                        Line = $lineNumber
                        Text = $line.TrimStart()
                    })
            }
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-StagedRustPanicMacroCandidates {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $result = Invoke-Native -FileName "git" -Arguments (@("grep", "--cached", "-n", "-E", $grepPattern, "--") + $Pathspecs)
    if ($result.ExitCode -eq 1) {
        return @()
    }
    if ($result.ExitCode -ne 0) {
        throw "git grep --cached failed:`n$($result.Output)"
    }

    $candidates = [System.Collections.Generic.List[object]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($line -notmatch "^(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
            throw "Unable to parse git grep output line: $line"
        }

        $text = $Matches["text"]
        if (Test-RustPanicMacroLine -Line $text) {
            [void]$candidates.Add([pscustomobject]@{
                    File = $Matches["file"]
                    Line = [int]$Matches["line"]
                    Text = $text.TrimStart()
                })
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-HeadRustPanicMacroCandidates {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$Pathspecs)

    if ($Pathspecs.Count -eq 0) {
        return @()
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $result = Invoke-Native -FileName "git" -Arguments (@("grep", "-n", "-E", $grepPattern, "HEAD", "--") + $Pathspecs)
    if ($result.ExitCode -eq 1) {
        return @()
    }
    if ($result.ExitCode -ne 0) {
        if ($result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return @()
        }
        throw "git grep HEAD failed:`n$($result.Output)"
    }

    $candidates = [System.Collections.Generic.List[object]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($line -notmatch "^HEAD:(?<file>[^:]+):(?<line>\d+):(?<text>.*)$") {
            throw "Unable to parse git grep HEAD output line: $line"
        }

        $text = $Matches["text"]
        if (Test-RustPanicMacroLine -Line $text) {
            [void]$candidates.Add([pscustomobject]@{
                    File = $Matches["file"]
                    Line = [int]$Matches["line"]
                    Text = $text.TrimStart()
                })
        }
    }

    return [object[]]$candidates.ToArray()
}

function Get-HeadText {
    param([Parameter(Mandatory = $true)][string]$Path)

    $result = Invoke-Native -FileName "git" -Arguments @("show", "HEAD:$Path")
    if ($result.ExitCode -ne 0) {
        return $null
    }

    $result.Stdout
}

function Get-RustTestContextMarkerSignature {
    param([AllowNull()][string]$Content)

    if ([string]::IsNullOrEmpty($Content)) {
        return ""
    }

    $markers = [System.Collections.Generic.List[string]]::new()
    $lineNumber = 0
    foreach ($line in $Content -split "`r?`n") {
        $lineNumber++
        $trimmed = $line.Trim()
        if ((Test-RustCfgTestAttributeLine -Line $trimmed) -or (Test-RustDirectTestAttributeLine -Line $trimmed)) {
            [void]$markers.Add("${lineNumber}:$trimmed")
        }
    }

    $markers -join "`n"
}

function Get-RustTestContextMarkerSignatureFromGit {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles,
        [switch]$Head
    )

    if ($CandidateFiles.Count -eq 0) {
        return ""
    }

    $grepPattern = "#\[[[:space:]]*(cfg[[:space:]]*\(|test|tokio::test|async_std::test|rstest)"
    $arguments = @("grep")
    if ($Head) {
        $arguments += @("-n", "-E", $grepPattern, "HEAD")
    } elseif (-not $script:InspectWorktree) {
        $arguments += @("--cached", "-n", "-E", $grepPattern)
    } else {
        $arguments += @("-n", "-E", $grepPattern)
    }
    $arguments += @("--") + $CandidateFiles

    $result = Invoke-Native -FileName "git" -Arguments $arguments
    if ($result.ExitCode -eq 1) {
        return ""
    }
    if ($result.ExitCode -ne 0) {
        if ($Head -and $result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
            return ""
        }
        throw "git grep test context markers failed:`n$($result.Output)"
    }

    $markers = [System.Collections.Generic.List[string]]::new()
    foreach ($rawLine in $result.Stdout.Split("`n", [System.StringSplitOptions]::RemoveEmptyEntries)) {
        $line = $rawLine.TrimEnd("`r")
        if ($Head -and $line.StartsWith("HEAD:", [System.StringComparison]::Ordinal)) {
            $line = $line.Substring("HEAD:".Length)
        }
        [void]$markers.Add($line)
    }

    $markerArray = [string[]]$markers
    [array]::Sort($markerArray, [System.StringComparer]::Ordinal)
    $markerArray -join "`n"
}

function Test-RustTestContextMarkersChanged {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles)

    (Get-RustTestContextMarkerSignatureFromGit -CandidateFiles $CandidateFiles) -ne
    (Get-RustTestContextMarkerSignatureFromGit -CandidateFiles $CandidateFiles -Head)
}

function Test-RustPanicMacroDiffChanged {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles)

    if ($CandidateFiles.Count -eq 0) {
        return $false
    }

    if ($script:InspectWorktree) {
        $tracked = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
        foreach ($file in (Get-IndexFiles -Pathspecs $CandidateFiles)) {
            [void]$tracked.Add($file)
        }
        foreach ($file in $CandidateFiles) {
            if (-not $tracked.Contains($file)) {
                return $true
            }
        }
    }

    $grepPattern = "(^|[^[:alnum:]_])(panic|todo|unimplemented|unreachable)[[:space:]]*(/\*[^*]*\*+([^/*][^*]*\*+)*/[[:space:]]*)*!"
    $arguments = @("diff")
    if ($script:InspectWorktree) {
        $arguments += "HEAD"
    } else {
        $arguments += "--cached"
    }
    $arguments += @("--quiet", "-G", $grepPattern, "--") + $CandidateFiles
    $result = Invoke-Native -FileName "git" -Arguments $arguments
    if ($result.ExitCode -eq 0) {
        return $false
    }
    if ($result.ExitCode -eq 1) {
        return $true
    }
    if ($script:InspectWorktree -and $result.Output -match "unknown revision|bad revision|ambiguous argument 'HEAD'|Not a valid object name HEAD") {
        return $true
    }

    throw "git diff panic macro check failed:`n$($result.Output)"
}

function Select-ProductionRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Candidates,
        [switch]$Head
    )

    $productionCandidates = [System.Collections.Generic.List[object]]::new()
    if ($Candidates.Count -eq 0) {
        return @()
    }

    $candidateFiles = [string[]]@($Candidates | ForEach-Object { $_.File } | Sort-Object -Unique)
    foreach ($file in $candidateFiles) {
        $content = if ($Head) { Get-HeadText -Path $file } else { Get-PolicyText -Path $file }
        if ($null -eq $content) {
            continue
        }

        $fileCandidates = @($Candidates | Where-Object { $_.File -eq $file } | Sort-Object -Property Line)
        if ($fileCandidates.Count -eq 0) {
            continue
        }

        $lineNumbers = [int[]]@($fileCandidates | ForEach-Object { [int]$_.Line })
        $executableMacroLines = Get-RustExecutablePanicMacroLineSet -Content $content -CandidateLines $lineNumbers
        $testCandidateLines = Get-RustCandidateTestLineSet -Content $content -CandidateLines $lineNumbers
        foreach ($candidate in $fileCandidates) {
            if (-not $executableMacroLines.Contains($candidate.Line)) {
                continue
            }
            if ($testCandidateLines.Contains($candidate.Line)) {
                continue
            }

            [void]$productionCandidates.Add($candidate)
        }
    }

    return [object[]]$productionCandidates.ToArray()
}

function Get-RustPanicCandidateKey {
    param(
        [Parameter(Mandatory = $true)]$Candidate,
        [switch]$IncludeLine
    )

    if ($IncludeLine) {
        return "$($Candidate.File)`0$($Candidate.Line)`0$($Candidate.Text)"
    }

    "$($Candidate.File)`0$($Candidate.Text)"
}

function Get-NewRustPanicMacroCandidates {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Candidates,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$CandidateFiles,
        [Nullable[bool]]$TestContextMarkersChanged = $null
    )

    if ($Candidates.Count -eq 0) {
        return @()
    }

    $contextMarkersChanged = if ($null -eq $TestContextMarkersChanged) {
        Test-RustTestContextMarkersChanged -CandidateFiles $CandidateFiles
    } else {
        [bool]$TestContextMarkersChanged
    }

    if ($contextMarkersChanged) {
        $productionCandidates = @(Select-ProductionRustPanicMacroCandidates -Candidates $Candidates)
        if ($productionCandidates.Count -eq 0) {
            return @()
        }

        $headCandidates = @(Get-HeadRustPanicMacroCandidates -Pathspecs $CandidateFiles)
        $productionHeadCandidates = @(Select-ProductionRustPanicMacroCandidates -Candidates $headCandidates -Head)
        $headProductionCounts = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
        foreach ($candidate in $productionHeadCandidates) {
            $key = Get-RustPanicCandidateKey -Candidate $candidate
            if ($headProductionCounts.ContainsKey($key)) {
                $headProductionCounts[$key]++
            } else {
                $headProductionCounts[$key] = 1
            }
        }

        $newProductionCandidates = [System.Collections.Generic.List[object]]::new()
        foreach ($candidate in @($productionCandidates | Sort-Object -Property File, Line)) {
            $key = Get-RustPanicCandidateKey -Candidate $candidate
            if ($headProductionCounts.ContainsKey($key) -and $headProductionCounts[$key] -gt 0) {
                $headProductionCounts[$key]--
                continue
            }

            [void]$newProductionCandidates.Add($candidate)
        }

        return [object[]]$newProductionCandidates.ToArray()
    }

    $headCandidates = Get-HeadRustPanicMacroCandidates -Pathspecs $CandidateFiles
    $headCounts = [System.Collections.Generic.Dictionary[string, int]]::new([System.StringComparer]::Ordinal)
    foreach ($candidate in $headCandidates) {
        $key = Get-RustPanicCandidateKey -Candidate $candidate -IncludeLine
        if ($headCounts.ContainsKey($key)) {
            $headCounts[$key]++
        } else {
            $headCounts[$key] = 1
        }
    }

    $newCandidates = [System.Collections.Generic.List[object]]::new()
    foreach ($candidate in @($Candidates | Sort-Object -Property File, Line)) {
        $key = Get-RustPanicCandidateKey -Candidate $candidate -IncludeLine
        if ($headCounts.ContainsKey($key) -and $headCounts[$key] -gt 0) {
            $headCounts[$key]--
            continue
        }

        [void]$newCandidates.Add($candidate)
    }

    return [object[]]$newCandidates.ToArray()
}

function Test-RustAddedPanicPatterns {
    $changedProductionRustFiles = [string[]]@($script:StagedFiles | Where-Object { Test-ProductionRustSourcePath -Path $_ })
    if ($changedProductionRustFiles.Count -eq 0) {
        Skip "Rust panic patterns" "no production Rust files staged"
        return
    }

    $violations = [System.Collections.Generic.List[string]]::new()
    $candidates = @()
    $candidatesPreFilteredToProduction = $false

    if ($script:InspectWorktree) {
        $scannableFiles = [string[]]@($changedProductionRustFiles | Where-Object {
                Test-Path -LiteralPath (Join-Path $script:RepoRoot $_)
            })
        $candidates = @(Get-WorktreeRustPanicMacroCandidates -Pathspecs $scannableFiles)
    } else {
        try {
            $candidates = @(Get-StagedRustPanicMacroCandidates -Pathspecs $changedProductionRustFiles)
        } catch {
            Fail "Rust panic patterns" $_.Exception.Message
            return
        }
    }

    if ($candidates.Count -eq 0) {
        Pass "Rust panic patterns"
        return
    }

    $candidateFiles = [string[]]@($candidates | ForEach-Object { $_.File } | Sort-Object -Unique)
    try {
        $testContextMarkersChanged = Test-RustTestContextMarkersChanged -CandidateFiles $candidateFiles
        if (-not $testContextMarkersChanged) {
            $panicMacroDiffChanged = Test-RustPanicMacroDiffChanged -CandidateFiles $candidateFiles
            if (-not $panicMacroDiffChanged) {
                Pass "Rust panic patterns"
                return
            }
        }

        $candidates = @(Get-NewRustPanicMacroCandidates -Candidates $candidates -CandidateFiles $candidateFiles -TestContextMarkersChanged:$testContextMarkersChanged)
        $candidatesPreFilteredToProduction = $testContextMarkersChanged
    } catch {
        Fail "Rust panic patterns" $_.Exception.Message
        return
    }
    if ($candidates.Count -eq 0) {
        Pass "Rust panic patterns"
        return
    }
    $candidateFiles = [string[]]@($candidates | ForEach-Object { $_.File } | Sort-Object -Unique)

    if (-not $script:InspectWorktree -and $candidateFiles.Count -gt 2) {
        try {
            Add-IndexTextCache -Pathspecs $candidateFiles
        } catch {
            Fail "Rust panic patterns" $_.Exception.Message
            return
        }
    }

    foreach ($file in @($candidateFiles | Sort-Object)) {
        $fileCandidates = @($candidates | Where-Object { $_.File -eq $file } | Sort-Object -Property Line)
        if ($fileCandidates.Count -eq 0) {
            continue
        }

        if ($candidatesPreFilteredToProduction) {
            foreach ($candidate in $fileCandidates) {
                [void]$violations.Add("$($candidate.File):$($candidate.Line): $($candidate.Text)")
                if ($violations.Count -ge 5) {
                    break
                }
            }
            if ($violations.Count -ge 5) {
                break
            }
            continue
        }

        $content = Get-PolicyText -Path $file
        if ($null -eq $content) {
            continue
        }

        $lineNumbers = [int[]]@($fileCandidates | ForEach-Object { [int]$_.Line })
        $executableMacroLines = Get-RustExecutablePanicMacroLineSet -Content $content -CandidateLines $lineNumbers
        $testCandidateLines = Get-RustCandidateTestLineSet -Content $content -CandidateLines $lineNumbers
        foreach ($candidate in $fileCandidates) {
            if (-not $executableMacroLines.Contains($candidate.Line)) {
                continue
            }
            if ($testCandidateLines.Contains($candidate.Line)) {
                continue
            }

            [void]$violations.Add("$($candidate.File):$($candidate.Line): $($candidate.Text)")
            if ($violations.Count -ge 5) {
                break
            }
        }
        if ($violations.Count -ge 5) {
            break
        }
    }

    if ($violations.Count -gt 0) {
        Fail "Rust panic patterns" "Changed production Rust files contain explicit panic macros. Replace panic!/todo!/unimplemented!/unreachable! with typed errors or move test-only macros behind #[cfg(test)]. Full .expect/.unwrap policy runs in local CI and CI.`n$($violations -join "`n")"
    } else {
        Pass "Rust panic patterns"
    }
}
