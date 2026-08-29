# Windows MSIX packaging

The package is a full-trust, packaged-classic desktop application. It keeps the
existing Win32 Default Programs registration because Windows reserves the
`http` and `https` URI schemes: `windows.protocol` declarations for either name
are ignored.

## Partner Center setup

Reserve the app name in Partner Center and copy these identity values from
**Product management > Product identity**:

- Package/Identity/Name
- Package/Identity/Publisher
- Package/Properties/PublisherDisplayName

The publisher string must be passed exactly as Partner Center provides it.

## Build

Run from a Windows Developer PowerShell with Rust's MSVC target and the Windows
SDK installed:

```powershell
.\package\windows\build-msix.ps1 `
  -Version 0.1.0.0 `
  -IdentityName 'PartnerCenter.Identity.Name' `
  -Publisher 'CN=PARTNER-CENTER-PUBLISHER-ID' `
  -PublisherDisplayName 'Your publisher name'
```

The script builds with Cargo, assembles the package layout, validates it with
MakeAppx, and writes `target\msix\Overbrowsered-<version>-<arch>.msix`.

GitHub Actions builds both architectures and combines them into one
`Overbrowsered-<version>.msixbundle`. The workflow defaults to the Store
identity assigned to Overbrowsered:

- `Package/Identity/Name`: `BrianMisiak.Overbrowsered`
- `Package/Identity/Publisher`: `CN=760D6692-461D-4E61-9DFA-33EF60598E9B`
- `Package/Properties/PublisherDisplayName`: `Brian Misiak`

The following non-secret repository variables can override that identity when
building a separate direct-distribution package:

- `WINDOWS_PACKAGE_IDENTITY`
- `WINDOWS_PACKAGE_PUBLISHER`
- `WINDOWS_PUBLISHER_DISPLAY_NAME`

If the variables are absent, CI produces an unsigned Store-ready artifact.
Partner Center signs accepted Store packages. Direct GitHub distribution must
use a separately configured publisher identity and a publicly trusted signing
certificate; an unsigned package will not install for ordinary users.

To create the bundle locally after building both architecture packages:

```powershell
.\package\windows\build-msixbundle.ps1 -Version 0.1.0.0
```

An unsigned package is intended for Partner Center, which signs accepted MSIX
packages. For local installation, pass the thumbprint of a certificate whose
subject exactly matches `Publisher`:

```powershell
.\package\windows\build-msix.ps1 <arguments above> `
  -CertificateThumbprint 'CERTIFICATE-SHA1-THUMBPRINT'
```

## Restricted capability

The manifest declares `unvirtualizedResources` and disables HKCU registry-write
virtualization. Without it, the registration written by `register_as_link_handler`
is private to the package and Overbrowsered does not appear in Windows Default
Apps.

This restricted capability must be declared in the Partner Center submission.
Explain that Overbrowsered is a user-selected web-link broker and must expose
its per-user `RegisteredApplications`, `Capabilities`, and ProgID keys to the
Windows shell. The app never changes the user's `UserChoice` value; it opens the
system Default Apps UI and leaves selection to the user.

Unvirtualized registry values persist after package removal. A future release
should add an explicit package-removal cleanup path if Windows exposes a reliable
hook suitable for Store applications. Do not add a manifest `windows.protocol`
fallback for `http` or `https`; Windows silently ignores those reserved schemes.

