@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1
cd /d "C:\Users\gaelc\Downloads\dorian\spotify-lite"
"C:\Users\gaelc\.cargo\bin\cargo.exe" %*
