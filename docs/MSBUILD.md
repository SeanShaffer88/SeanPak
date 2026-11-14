# Visual Studio and MSBuild integration

SeanPak can run after a .NET SDK-style project publishes. The target below is only an adapter: it calls the existing SeanPak CLI, so manifest validation, package naming, compression, checksums, and output behavior stay in one implementation.

## Add the target

Place SeanPak's Windows x64 CLI at tools/seanpak.exe, keep a valid manifest.json beside the project, and import the target near the end of the project file:

~~~xml
<PropertyGroup>
  <SeanPakEnabled>true</SeanPakEnabled>
  <SeanPakExecutable>$(MSBuildProjectDirectory)\tools\seanpak.exe</SeanPakExecutable>
  <SeanPakManifest>$(MSBuildProjectDirectory)\manifest.json</SeanPakManifest>
  <SeanPakOutputDirectory>$(MSBuildProjectDirectory)\dist</SeanPakOutputDirectory>
</PropertyGroup>

<Import Project="$(MSBuildProjectDirectory)\msbuild\SeanPak.targets" />
~~~

Run the normal publish target:

~~~powershell
dotnet publish -c Release -p:SeanPakEnabled=true
~~~

When Publish completes, SeanPak packages the contents of $(PublishDir) and writes the Setup.exe, Portable.zip, and SHA256SUMS.txt to dist. The manifest's primaryExe path must be relative to that published-files directory.

## Properties

- SeanPakEnabled defaults to false, so importing the file does not change an existing build until enabled.
- SeanPakExecutable defaults to tools\seanpak.exe under the project directory.
- SeanPakManifest defaults to manifest.json under the project directory.
- SeanPakFilesDirectory defaults to $(PublishDir) when the publish target completes; set it explicitly only when the payload is elsewhere.
- SeanPakOutputDirectory defaults to dist under the project directory.
- SeanPakForce defaults to false. Set it to true to pass --force and replace existing output files.

The target fails the publish if the CLI, manifest, or payload directory is missing, or if SeanPak returns a nonzero exit code. Paths with spaces are quoted. The target does not download the CLI, sign your package, or add a second packaging implementation.

## CI runners

The target only requires a Windows x64 environment with MSBuild/.NET and the SeanPak CLI; it does not call GitHub-specific APIs. This repository's automated smoke test validates the target on a Windows GitHub Actions runner. Jenkins, Docker, and non-Windows hosts have not been validated here and are not claimed as supported by this integration.
