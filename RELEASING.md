# Releasing Overbrowsered

Ordinary pushes and pull requests run unsigned, path-filtered verification. Release credentials are
only available to jobs using the protected `release` GitHub environment.

## Cut a release

Start from an up-to-date `master`, then push an annotated semantic-version tag:

```console
git switch master
git pull --ff-only
git tag -a v1.5.0 -m "Overbrowsered 1.5.0"
git push origin v1.5.0
```

The tag must have exactly three numeric components. The release workflow overrides
`MARKETING_VERSION` from the tag and assigns a unique App Store build number from the GitHub workflow
run and attempt numbers.

After the protected environment is approved, the workflow:

1. creates a Developer ID archive, notarizes and staples it;
2. uploads a separately signed build to App Store Connect; and
3. creates a draft GitHub release containing the notarized direct-download ZIP and checksum.

Publishing the GitHub release and submitting the uploaded build for App Review remain deliberate
manual actions.

## GitHub release environment

The `release` environment uses these non-secret variables:

- `APPLE_TEAM_ID`
- `MACOS_BUNDLE_ID`
- `ASC_KEY_ID`
- `ASC_ISSUER_ID`

It uses these secrets:

- `ASC_PRIVATE_KEY`: contents of the App Store Connect `AuthKey_*.p8` file.
- `DEVELOPER_ID_P12`: base64 of a Developer ID Application certificate and private key exported as
  PKCS#12.
- `DEVELOPER_ID_P12_PASSWORD`: password used for that PKCS#12 export.
- `APP_STORE_APPLICATION_P12`: base64 of a Mac App Distribution certificate and private key.
- `APP_STORE_APPLICATION_P12_PASSWORD`: password used for that PKCS#12 export.
- `APP_STORE_INSTALLER_P12`: base64 of a Mac Installer Distribution certificate and private key.
- `APP_STORE_INSTALLER_P12_PASSWORD`: password used for that PKCS#12 export.
- `APP_STORE_PROVISIONING_PROFILE`: base64 of the Mac App Store distribution provisioning profile
  for `com.bmisiak.Overbrowsered`.

On macOS, copy a file as base64 with:

```console
base64 -i certificate.p12 | pbcopy
base64 -i Overbrowsered.provisionprofile | pbcopy
```

Copy the `.p8` contents directly rather than base64-encoding that secret.

The App Store Connect key authenticates uploads and notarization. It does not sign the application;
the PKCS#12 files contain the signing private keys.

Use a dedicated **team** App Store Connect API key with the `Developer` role for
`ASC_KEY_ID`, `ASC_ISSUER_ID`, and `ASC_PRIVATE_KEY`. Apple doesn't allow individual API keys to
use `notarytool`, and team keys can't be restricted to one app. The `Developer` role is therefore
the narrowest practical one-key setup for both build upload and notarization; revoke this key if the
GitHub release setup is retired or compromised.

Prefer release-specific signing certificates where Apple's certificate limits permit it:

- one Developer ID Application certificate for the direct download;
- one Mac App Distribution certificate for the App Store application; and
- one Mac Installer Distribution certificate for the App Store upload.

Export each certificate together with its private key as a password-protected `.p12`. Create a Mac
App Store distribution provisioning profile for `com.bmisiak.Overbrowsered` using the Mac App
Distribution certificate, then store its base64 form in `APP_STORE_PROVISIONING_PROFILE`.
