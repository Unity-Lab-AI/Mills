@echo off
rem Build localhost/pgm-re-base:<version> and :latest on Windows (Docker Desktop or Podman Desktop).
rem   build.cmd            build
rem   build.cmd --test     build, then run the smoke test inside a container (needs the image only)
rem   set ENGINE=podman    choose the engine (default: podman if found, else docker)
setlocal
set "HERE=%~dp0"
set /p VERSION=<"%HERE%VERSION"
set "NAME=localhost/pgm-re-base"
if "%ENGINE%"=="" (
  where podman >nul 2>nul && (set "ENGINE=podman") || (set "ENGINE=docker")
)
where %ENGINE% >nul 2>nul || (echo need podman or docker 1>&2 & exit /b 1)
%ENGINE% build --file "%HERE%Containerfile" --build-arg "IMAGE_VERSION=%VERSION%" --tag "%NAME%:%VERSION%" --tag "%NAME%:latest" "%HERE%" || exit /b 1
%ENGINE% images "%NAME%"
if /i "%~1"=="--test" (
  rem smoke.sh is also inside the image, so Windows needs no shell: run it in a throwaway container.
  %ENGINE% run --rm --network none -e PGM_RE_QUIET=1 "%NAME%:%VERSION%" /opt/re-smoke.sh --inside || exit /b 1
)
endlocal
