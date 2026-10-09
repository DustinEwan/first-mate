---
name: powershell
description: Machine inventory through the terminal — hardware, drivers, devices, services, network, OS facts via CIM/WMI and read-only system queries. Use when the user wants to know what IS on/with the machine; tasks that CHANGE something with an existing UI go through that UI instead (winapp).
---

# PowerShell for Inventory

Use the terminal for the jobs terminals are good at. That means finding out
what IS on the machine, and batch work that has no UI. `run_command` with
`shell="powershell"` is ONE persistent session. `$vars`, functions, and cwd
persist across calls. The command text IS PowerShell. NEVER wrap it in
`powershell -Command "..."`. The session parses that wrapper's quotes and
`$vars` before PowerShell sees them. Your script then fails with no error
message.

## Inventory patterns (read-only, non-elevated)

```
# drivers (name, version, date) — the driver question, answered in one line
Get-CimInstance Win32_PnPSignedDriver | Select-Object DeviceName, DriverVersion, DriverDate, Manufacturer | Sort-Object DeviceName

# devices with problems
Get-CimInstance Win32_PnPEntity | Where-Object ConfigManagerErrorCode -ne 0 | Select-Object Name, ConfigManagerErrorCode

# OS / system facts
Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, LastBootUpTime
Get-CimInstance Win32_ComputerSystem   | Select-Object Manufacturer, Model

# third-party driver packages in the store
pnputil /enum-drivers

# installed programs
Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*' | Select-Object DisplayName, DisplayVersion
```

Admin-only cmdlets (`Get-WindowsDriver -Online`, DISM) fail with "requires
elevation". The CIM/WMI equivalents above answer the same questions without
elevation. If the task is to CHANGE something that needs admin, that is a UI
task. Open the surface and let Windows ask the user (winapp).

## Session craft

- Network cmdlets can block for minutes. Every command queued behind them
  waits. Pass a timeout: `Resolve-DnsName -TimeoutSeconds 2`.
- Wide output: use `Format-Table -AutoSize | Out-String -Width 200`, or
  select only the properties you need.
- Multi-line scripts, functions, and heredocs (`@'...'@`) persist in the
  session. Define one, then reuse it across calls.
- Processes: `tasklist | findstr /i <name>` beats pulling the full list.
