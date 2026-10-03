@echo off
REM Build and run the Java client against the local dev server.
REM Requirements: JDK 21+ (Temurin/Liberica), Apache Ant 1.10.x
REM The client auto-connects to 127.0.0.1 (auth 1871, game 1870, res 1872).
setlocal
cd /d "%~dp0.."

where ant >nul 2>nul
if errorlevel 1 (
    echo [ERROR] ant not found. Install Apache Ant 1.10.x and add it to PATH.
    exit /b 1
)

if not exist "build\haven.jar" (
    echo Building client jar...
    call ant jar
    if errorlevel 1 exit /b 1
)

echo Starting Haven client (localhost server)...
REM JOGL 1.1 loads its natives (jogl.dll, jogl_awt.dll, jogl_cg.dll,
REM gluegen-rt.dll) via System.loadLibrary, which searches only
REM java.library.path. Point it at the committed DLLs in build\ and
REM also prepend them to PATH so Windows can resolve dependent DLLs.
set "JOGL_NATIVE=%CD%\build"
set "PATH=%JOGL_NATIVE%;%PATH%"
REM Optional: add -Dhaven.autoplay=Player to skip the character list.
java --enable-native-access=ALL-UNNAMED -Djava.library.path="%JOGL_NATIVE%" -cp "build\haven.jar;lib\*;build\res" haven.MainFrame %*
endlocal
