set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE static)
set(VCPKG_LIBRARY_LINKAGE static)
# build.rs and the native fixtures always link the Release libraries.
set(VCPKG_BUILD_TYPE release)
