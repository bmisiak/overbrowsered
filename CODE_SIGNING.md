# Code signing policy

Free code signing provided by SignPath.io, certificate by SignPath Foundation.

## Official project locations

- Source code: https://github.com/bmisiak/overbrowsered
- Release downloads: https://github.com/bmisiak/overbrowsered/releases
- Issue tracker: https://github.com/bmisiak/overbrowsered/issues

Do not trust binaries presented as official Overbrowsered releases if they do not come from the locations listed above.

## Build and signing process

Windows release artifacts are built from this public repository and its checked-in build scripts by GitHub Actions on GitHub-hosted runners. Release artifacts intended for direct distribution are submitted to SignPath from that trusted build and require manual approval before signing.

Microsoft Store packages are submitted through Partner Center and are signed separately by Microsoft for Store distribution.

## Project roles

- Committer and reviewer: Brian Misiak ([@bmisiak](https://github.com/bmisiak))
- Release approver: Brian Misiak ([@bmisiak](https://github.com/bmisiak))

Changes are reviewed through the public GitHub commit and pull-request history. The release approver checks the source revision and build before approving a signing request.

## Privacy

Overbrowsered does not collect or transmit personal information. On Windows, it stores only the ProgID of the most recently used browser locally under the current user's registry.

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it.
