# Install script for directory: /home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/src

# Set the install prefix
if(NOT DEFINED CMAKE_INSTALL_PREFIX)
  set(CMAKE_INSTALL_PREFIX "/usr/local")
endif()
string(REGEX REPLACE "/$" "" CMAKE_INSTALL_PREFIX "${CMAKE_INSTALL_PREFIX}")

# Set the install configuration name.
if(NOT DEFINED CMAKE_INSTALL_CONFIG_NAME)
  if(BUILD_TYPE)
    string(REGEX REPLACE "^[^A-Za-z0-9_]+" ""
           CMAKE_INSTALL_CONFIG_NAME "${BUILD_TYPE}")
  else()
    set(CMAKE_INSTALL_CONFIG_NAME "RelWithDebInfo")
  endif()
  message(STATUS "Install configuration: \"${CMAKE_INSTALL_CONFIG_NAME}\"")
endif()

# Set the component getting installed.
if(NOT CMAKE_INSTALL_COMPONENT)
  if(COMPONENT)
    message(STATUS "Install component: \"${COMPONENT}\"")
    set(CMAKE_INSTALL_COMPONENT "${COMPONENT}")
  else()
    set(CMAKE_INSTALL_COMPONENT)
  endif()
endif()

# Install shared libraries without execute permission?
if(NOT DEFINED CMAKE_INSTALL_SO_NO_EXE)
  set(CMAKE_INSTALL_SO_NO_EXE "1")
endif()

# Is this installation the result of a crosscompile?
if(NOT DEFINED CMAKE_CROSSCOMPILING)
  set(CMAKE_CROSSCOMPILING "FALSE")
endif()

# Set path to fallback-tool for dependency-resolution.
if(NOT DEFINED CMAKE_OBJDUMP)
  set(CMAKE_OBJDUMP "/usr/bin/objdump")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  foreach(file
      "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsecp256k1.so.5.0.1"
      "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsecp256k1.so.5"
      )
    if(EXISTS "${file}" AND
       NOT IS_SYMLINK "${file}")
      file(RPATH_CHECK
           FILE "${file}"
           RPATH "")
    endif()
  endforeach()
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/lib/libsecp256k1.so.5.0.1"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/lib/libsecp256k1.so.5"
    )
  foreach(file
      "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsecp256k1.so.5.0.1"
      "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/libsecp256k1.so.5"
      )
    if(EXISTS "${file}" AND
       NOT IS_SYMLINK "${file}")
      if(CMAKE_INSTALL_DO_STRIP)
        execute_process(COMMAND "/usr/bin/strip" "${file}")
      endif()
    endif()
  endforeach()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib" TYPE SHARED_LIBRARY FILES "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/lib/libsecp256k1.so")
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/include" TYPE FILE FILES
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/secp256k1.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_preallocated.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_ecdh.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_extrakeys.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_schnorrsig.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_musig.h"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/include/rustsecp256k1_v0_10_0_ellswift.h"
    )
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  if(EXISTS "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1/libsecp256k1-targets.cmake")
    file(DIFFERENT _cmake_export_file_changed FILES
         "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1/libsecp256k1-targets.cmake"
         "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/CMakeFiles/Export/c49d8531aec3e1dc15923280032fb77d/libsecp256k1-targets.cmake")
    if(_cmake_export_file_changed)
      file(GLOB _cmake_old_config_files "$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1/libsecp256k1-targets-*.cmake")
      if(_cmake_old_config_files)
        string(REPLACE ";" ", " _cmake_old_config_files_text "${_cmake_old_config_files}")
        message(STATUS "Old export file \"$ENV{DESTDIR}${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1/libsecp256k1-targets.cmake\" will be replaced.  Removing files [${_cmake_old_config_files_text}].")
        unset(_cmake_old_config_files_text)
        file(REMOVE ${_cmake_old_config_files})
      endif()
      unset(_cmake_old_config_files)
    endif()
    unset(_cmake_export_file_changed)
  endif()
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1" TYPE FILE FILES "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/CMakeFiles/Export/c49d8531aec3e1dc15923280032fb77d/libsecp256k1-targets.cmake")
  if(CMAKE_INSTALL_CONFIG_NAME MATCHES "^([Rr][Ee][Ll][Ww][Ii][Tt][Hh][Dd][Ee][Bb][Ii][Nn][Ff][Oo])$")
    file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1" TYPE FILE FILES "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/CMakeFiles/Export/c49d8531aec3e1dc15923280032fb77d/libsecp256k1-targets-relwithdebinfo.cmake")
  endif()
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib/cmake/libsecp256k1" TYPE FILE FILES
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/libsecp256k1-config.cmake"
    "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/libsecp256k1-config-version.cmake"
    )
endif()

if(CMAKE_INSTALL_COMPONENT STREQUAL "Unspecified" OR NOT CMAKE_INSTALL_COMPONENT)
  file(INSTALL DESTINATION "${CMAKE_INSTALL_PREFIX}/lib/pkgconfig" TYPE FILE FILES "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/libsecp256k1.pc")
endif()

string(REPLACE ";" "\n" CMAKE_INSTALL_MANIFEST_CONTENT
       "${CMAKE_INSTALL_MANIFEST_FILES}")
if(CMAKE_INSTALL_LOCAL_ONLY)
  file(WRITE "/home/pyth/bwk/.claude/worktrees/sp-sync-bench/vendor/secp256k1-sys/depend/secp256k1/build-phase6/src/install_local_manifest.txt"
     "${CMAKE_INSTALL_MANIFEST_CONTENT}")
endif()
