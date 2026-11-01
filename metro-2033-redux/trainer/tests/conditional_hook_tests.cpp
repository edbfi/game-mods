// SPDX-License-Identifier: AGPL-3.0-only
#include "metro/conditional_hooks.hpp"
#include <sys/mman.h>
#include <cassert>
#include <cstring>
#include <iostream>
struct State { std::uint64_t ax,bx,cx,dx,r8,r9,flags; float xmm9[4]; };
extern "C" void run_hook(void*,State*);
asm(R"(
.text
.global run_hook
run_hook:
 push %rbx
 push %r12
 push %r13
 push %r14
 push %rdi
 sub $32,%rsp
 mov %rdi,%r12
 mov %rsi,%r13
 mov 0(%r13),%rax
 mov 8(%r13),%rbx
 mov 16(%r13),%rcx
 mov 24(%r13),%rdx
 mov 32(%r13),%r8
 mov 40(%r13),%r9
 movups 56(%r13),%xmm9
 mov %rcx,%r14
 mov $2,%edi
 pushq 48(%r13)
 popfq
 call *%r12
 pushfq
 popq 48(%r13)
 mov %rax,0(%r13)
 mov %rbx,8(%r13)
 mov %rcx,16(%r13)
 mov %rdx,24(%r13)
 mov %r8,32(%r13)
 mov %r9,40(%r13)
 movups %xmm9,56(%r13)
 add $32,%rsp
 pop %rdi
 pop %r14
 pop %r13
 pop %r12
 pop %rbx
 ret
)");
int main() {
 auto* m=static_cast<unsigned char*>(mmap(nullptr,0x400000,7,MAP_PRIVATE|MAP_ANONYMOUS,-1,0));assert(m!=MAP_FAILED);
 auto base=reinterpret_cast<std::uintptr_t>(m);
 auto put=[&](std::size_t at,std::initializer_list<unsigned char> b){std::copy(b.begin(),b.end(),m+at);};
 alignas(16) unsigned char object[0x1400]{},vt[0x200]{};
 auto obj=reinterpret_cast<std::uintptr_t>(object),table=reinterpret_cast<std::uintptr_t>(vt);
 std::memcpy(object,&table,8);auto stub=base+0x2000;std::memcpy(vt+0x1b0,&stub,8);
 for(const auto& site:metro::verified_conditional_sites()) {
  auto image=metro::make_conditional_image(base,base+0x100,base+0x1000,site);
  std::memcpy(m+0x100,image.code.data(),image.code.size());
  std::memcpy(m+site.rva,image.hook.replacement.data(),image.hook.replacement.size());auto end=site.rva+site.original.size();put(end,{0xc3});
  if(site.rva==0x2aecd0)put(end,{0xb8,0,0,0,0,0xc3});
  if(site.rva==0x1e3714){put(end,{0x0f,0x94,0xc0,0xc3});put(0x1e372d,{0xb8,1,0,0,0,0xc3});}
  put(0x2000,{0x48,0x8b,0x04,0x24,0xc3});
  if(site.original[0]==0xe8){std::int32_t d;std::memcpy(&d,site.original.data()+1,4);auto dest=static_cast<std::size_t>(static_cast<std::int64_t>(end)+d);put(dest,{0x48,0x8b,0x04,0x24,0xc3});if(std::string_view(site.feature)=="unlimited_throwables")put(dest+0x30,{0x48,0x8b,0x04,0x24,0xc3});}
  for(unsigned choice=0;choice<3;++choice){
   const bool on=choice==1;
   const char* names[]={"unlimited_ammo","unlimited_throwables","unlimited_medkits","unlimited_filters","indestructible_mask","god_mode","unlimited_flashlight"};unsigned bit=0;while(std::string_view(names[bit])!=site.feature)++bit;
   *reinterpret_cast<std::uint32_t*>(m+0x1000)=on?(1u<<bit):(choice==2?0x80000000u:0);
   for(unsigned equal=0;equal<2;++equal){
    State s{7,7,obj,3,obj,12345,0x247,{2,3,4,5}};
    *reinterpret_cast<std::uint32_t*>(object+0x478)=0xff;object[0x1344]=equal?2:0;
    if(site.rva==0x32b6e9)s.ax=table;
    const float four=4;asm volatile("movss %0,%%xmm2"::"m"(four):"xmm2");run_hook(m+site.rva,&s);
    assert(s.r9==12345&&s.r8==obj&&s.cx==obj&&s.dx==3);
    assert(s.bx==(site.rva==0x2b9a55&&!on?5u:7u));
    if(site.rva==0x3292d1){assert(s.xmm9[0]==(on?0:8));assert(s.xmm9[1]==(on?0:3));assert(s.xmm9[2]==(on?0:4)&&s.xmm9[3]==(on?0:5));}
    else assert(s.xmm9[0]==2&&s.xmm9[1]==3&&s.xmm9[2]==4&&s.xmm9[3]==5);
    if(site.rva==0x33079e)assert(*reinterpret_cast<std::uint32_t*>(object+0x478)==(on?0xffu:7u));
    if(site.rva==0x2b9a55)assert(s.ax==table);
    if(site.rva==0x2aecd0)assert((s.ax&0xff)==on);
    if(site.rva==0x1e3714)assert((s.ax&0xff)==(on||equal));
    if(site.original[0]==0xe8){bool skip=on&&std::string_view(site.feature)=="unlimited_flashlight";assert(s.ax==(skip?7:base+end));}
    if(site.rva==0x32b6e9)assert(s.ax==(on?table:base+end));
    if(site.rva==0x33079e) assert((s.flags&0x8c5)==(on?(0x247&0x8c5):0));
    else if(site.rva==0x2b9a55) assert((s.flags&0x8d5)==(on?(0x247&0x8d5):4));
    else if(site.rva==0x1e3714) assert((s.flags&0x8d5)==(on?(0x247&0x8d5):(equal?0x44:0x91)));
    else assert((s.flags&0x8d5)==(0x247&0x8d5));
   }
  }
 }
 munmap(m,0x400000);std::cout<<"All conditional paths: registers, SIMD, flags and original call continuations passed\n";
}
