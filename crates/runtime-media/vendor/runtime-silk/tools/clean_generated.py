#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Deterministic C2Rust postprocessing; no codec algorithm changes.
Input is decoder_amalgam.rs from pinned C2Rust 0.22.1, LLVM15, NDEBUG,
std=c99 generic fixed-point SDK. Run only as a developer management action.
"""
import pathlib,re,sys
source=pathlib.Path(sys.argv[1]).read_text()
start=source.index('extern "C" {')
depth=1;end=start+len('extern "C" {')
while depth:
 if source[end]=='{':depth+=1
 if source[end]=='}':depth-=1
 end+=1
source=source[:start]+source[end:]
source=source.replace('#[no_mangle]\n','')
# C2Rust emits const C tables as `static mut`. Never create Rust exclusive
# references to these read-only shared tables: retain raw-address reads.
statics={}
for m in re.finditer(r'(?:pub )?static mut (\w+)\s*:',source):
 pos=m.end();depth=0;end=pos
 while end<len(source):
  c=source[end]
  if c in '[<(':depth+=1
  elif c in ']> )'.replace(' ',''):depth-=1
  if c=='=' and depth==0:break
  end+=1
 statics[m[1]]=source[pos:end].strip()
def inner_array(t):
 t=t.strip()
 if not t.startswith('['):raise RuntimeError('expected array '+t)
 depth=0
 for i,c in enumerate(t[1:],1):
  if c in '[<(':depth+=1
  elif c in ']> )'.replace(' ',''):depth-=1
  if c==';' and depth==0:return t[1:i].strip()
 raise RuntimeError('bad array '+t)
pattern=re.compile(r'\b('+ '|'.join(sorted(map(re.escape,statics),key=len,reverse=True)) +r')\b')
edits=[]
for m in pattern.finditer(source):
 pos=m.end();indexes=0
 while True:
  while pos<len(source) and source[pos].isspace():pos+=1
  if source[pos:pos+1]!='[':break
  depth=1;pos+=1
  while depth:
   if source[pos]=='[':depth+=1
   if source[pos]==']':depth-=1
   pos+=1
  indexes+=1
 suffix=re.match(r'\s*\.as_(?:mut_)?ptr\(\)',source[pos:])
 if suffix:
  ty=statics[m[1]]
  for _ in range(indexes+1):ty=inner_array(ty)
  expr=source[m.start():pos].strip()
  edits.append((m.start(),pos+suffix.end(),f'::core::ptr::addr_of_mut!({expr}).cast::<{ty}>()'))
for start,end,text in reversed(edits):source=source[:start]+text+source[end:]
header='''// Generated from Skype SILK SDK 1.0.9.6, decoder dependency closure.
// Copyright (c) 2006-2012, Skype Limited. BSD-3-Clause terms in ../LICENSE.
// C2Rust 0.22.1 / LLVM15; no C compilation, native codec library or global ABI exports.
// Algorithm unchanged; memory operations use Rust pointer intrinsics and shared
// constant table addresses do not manufacture exclusive Rust references.
#![allow(non_camel_case_types,non_snake_case,non_upper_case_globals,unused_mut,unused_assignments,unused_parens,dead_code,static_mut_refs,unused_unsafe,unused_variables)]
unsafe fn memcpy(dst:*mut core::ffi::c_void,src:*const core::ffi::c_void,n:usize)->*mut core::ffi::c_void{core::ptr::copy_nonoverlapping(src.cast::<u8>(),dst.cast::<u8>(),n);dst}
unsafe fn memmove(dst:*mut core::ffi::c_void,src:*const core::ffi::c_void,n:usize)->*mut core::ffi::c_void{core::ptr::copy(src.cast::<u8>(),dst.cast::<u8>(),n);dst}
unsafe fn memset(dst:*mut core::ffi::c_void,value:core::ffi::c_int,n:usize)->*mut core::ffi::c_void{core::ptr::write_bytes(dst.cast::<u8>(),value as u8,n);dst}
'''
pathlib.Path(sys.argv[2]).write_text(header+source)
print('replaced',len(edits),'shared table pointer expressions')
