// SPDX-License-Identifier: AGPL-3.0-only
#include "metro/pressure.hpp"
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>
#include <csignal>
#include <cstring>
#include <cassert>
#include <iostream>
#include <array>
#include <limits>
#include <cmath>
#include <vector>
#include <xmmintrin.h>

static std::array<std::uint64_t, 2> continuation;
struct State { std::array<std::uint64_t,17> integer{}; std::array<std::array<std::uint32_t,4>,16> xmm{}; };
// Execute both paths from identical register seeds; capture at their return. The wrapper restores the host ABI afterwards.
static State capture(unsigned char* arena, std::uintptr_t target, std::uintptr_t weapon, bool recovery) {
 State out; alignas(16) std::array<std::uint64_t,128> stack; stack.fill(0xdeadbeef12345678ull); std::uintptr_t host_stack=0; std::vector<unsigned char> b;
 auto emit=[&](std::initializer_list<unsigned char> x){b.insert(b.end(),x);};
 auto imm=[&](std::uint64_t x){for(int i=0;i<8;++i)b.push_back(x>>(8*i));};
 auto mov=[&](unsigned r,std::uint64_t x){emit({static_cast<unsigned char>(0x48|(r>=8)),static_cast<unsigned char>(0xb8+(r%8))});imm(x);};
 // Save host GPRs, including callee-saved registers, below its return address.
 for(unsigned r=0;r<16;++r)if(r!=4){if(r>=8)emit({0x41});emit({static_cast<unsigned char>(0x50+r%8)});}
 mov(0,reinterpret_cast<std::uintptr_t>(&host_stack));emit({0x48,0x89,0x20});
 mov(4,reinterpret_cast<std::uintptr_t>(stack.data()+120));
 alignas(16) std::array<std::array<std::uint32_t,4>,16> seeds{};
 for(unsigned r=0;r<16;++r){seeds[r]={0x40800000u,0x12340000u+r,0x56780000u+r,0x9abc0000u+r};}
 mov(0,reinterpret_cast<std::uintptr_t>(seeds.data()));
 for(unsigned r=0;r<16;++r){emit({0xf3});if(r>=8)emit({0x44});emit({0x0f,0x6f,static_cast<unsigned char>(0x80|((r%8)<<3))});for(int i=0;i<4;++i)b.push_back((r*16)>>(8*i));}
 emit({0x68,0xd7,0x02,0,0,0x9d});
 for(unsigned r=0;r<16;++r)if(r!=4)mov(r,(r==1||r==3)?weapon:0x1234567800000000ull+r);
 // Indirect RIP-relative call leaves every seeded GPR intact.
 emit({0xff,0x15,0x02,0,0,0,0xeb,0x08});imm(target);
 // RSP plus all fifteen other GPRs and flags, in a stable stack layout.
 emit({0x9c});for(unsigned r=0;r<16;++r){if(r>=8)emit({0x41});emit({static_cast<unsigned char>(0x50+r%8)});}
 mov(7,reinterpret_cast<std::uintptr_t>(out.integer.data()));emit({0x48,0x89,0xe6});mov(1,17);emit({0xfc,0xf3,0x48,0xa5});
 mov(0,reinterpret_cast<std::uintptr_t>(out.xmm.data()));
 for(unsigned r=0;r<16;++r){emit({0xf3});if(r>=8)emit({0x44});emit({0x0f,0x7f,static_cast<unsigned char>(0x80|((r%8)<<3))});for(int i=0;i<4;++i)b.push_back((r*16)>>(8*i));}
 mov(0,reinterpret_cast<std::uintptr_t>(&host_stack));emit({0x48,0x8b,0x20});
 for(int r=15;r>=0;--r)if(r!=4){if(r>=8)emit({0x41});emit({static_cast<unsigned char>(0x58+r%8)});}
 emit({0xc3});
 // Recovery uses the shared native unwind tail before this capture.
 // Thus stack equality here checks net balance, not the intermediate prologue.
 (void)recovery;std::memcpy(arena,b.data(),b.size());reinterpret_cast<void(*)()>(arena)();
 // Capture itself uses 136 bytes; hook scratch and replay must stay inside
 // this explicitly bounded region. Upper words guard the call return boundary.
 for(unsigned i=0;i<96;++i)assert(stack[i]==0xdeadbeef12345678ull);
 for(unsigned i=120;i<128;++i)assert(stack[i]==0xdeadbeef12345678ull);
 if(recovery){assert(continuation[0]==reinterpret_cast<std::uintptr_t>(stack.data()+120)-48);assert(continuation[1]==weapon);}
 return out;
}

int main(){
 auto* m=static_cast<unsigned char*>(mmap(nullptr,0x1000000,7,MAP_PRIVATE|MAP_ANONYMOUS,-1,0));assert(m!=MAP_FAILED);
 auto base=reinterpret_cast<std::uintptr_t>(m);
 alignas(16) unsigned char w[0x1400]{},cfg[0x1400]{},owner[8]{},manager[0x40]{},player[0x100]{};
 auto ptr=[](void* p,void* v){std::memcpy(p,&v,8);};auto val=[](void*p,auto v){std::memcpy(p,&v,sizeof v);};
 ptr(w+0x118,owner);ptr(owner,player+0xa0);ptr(w+0x1b8,cfg);ptr(m+0xfbd820,manager);ptr(manager+0x38,player);val(cfg,42u);val(m+0xf5d0e8,42u);val(cfg+0x1320,10.8f);
 // Validate the inclusive signed rel32 endpoints independently of execution.
 for (auto site : {metro::PressureSite::Recovery, metro::PressureSite::Consumption1, metro::PressureSite::Consumption2}) {
  const std::uintptr_t origin=0x100000000ull;
  const auto next=metro::make_pressure_image(origin,origin,origin,site).hook.address+5;
  for (auto delta : {std::int64_t(INT32_MIN),std::int64_t(INT32_MAX)})
   assert(!metro::make_pressure_image(origin,next+delta,origin,site).code.empty());
  for (auto delta : {std::int64_t(INT32_MIN)-1,std::int64_t(INT32_MAX)+1}) {
   bool rejected=false;try{metro::make_pressure_image(origin,next+delta,origin,site);}catch(const std::out_of_range&){rejected=true;}assert(rejected);
  }
 }
 for (bool invalid_site : {false,true}) {
  bool rejected=false;try{metro::make_pressure_image(base,base+0x1000,base+0x800+(invalid_site?0:1),invalid_site?static_cast<metro::PressureSite>(99):metro::PressureSite::Recovery);}catch(const std::invalid_argument&){rejected=true;}assert(rejected);
 }
 const auto host_mxcsr=_mm_getcsr();
 unsigned cases=0, state_cases=0;
 for(auto site:{metro::PressureSite::Recovery,metro::PressureSite::Consumption1,metro::PressureSite::Consumption2}){
 auto image=metro::make_pressure_image(base,base+0x1000,base+0x800,site);std::memcpy(m+0x1000,image.code.data(),image.code.size());
 auto end=image.hook.address-base+image.hook.original.size();
 std::memcpy(m+0x4000,image.hook.original.data(),image.hook.original.size());
 m[0x4000+image.hook.original.size()]=0xe9;
 auto rel=static_cast<std::int32_t>(end-(0x4000+image.hook.original.size()+5));
 std::memcpy(m+0x4000+image.hook.original.size()+1,&rel,4);
 if(site==metro::PressureSite::Recovery){// At continuation, RSP is entry RSP - 40 and saved RBX is [RSP+32].
 std::vector<unsigned char> tail={0x52,0x50,0x48,0xb8};
 auto address=reinterpret_cast<std::uintptr_t>(continuation.data());
 for(unsigned i=0;i<8;++i)tail.push_back(address>>(8*i));
 const unsigned char record[]={0x48,0x8d,0x54,0x24,0x10,0x48,0x89,0x10,0x48,0x8b,0x54,0x24,0x30,0x48,0x89,0x50,0x08,0x58,0x5a,0x48,0x83,0xc4,0x20,0x5b,0xc3};
 // Preserve RDX too: record with absolute stores would otherwise alter it.
 // The continuation is shared by both native and emitted executions.
 tail.insert(tail.end(),std::begin(record),std::end(record));std::memcpy(m+end,tail.data(),tail.size());}else m[end]=0xc3;
 // Isolate unmasked exceptions: a native consumption SUBSS may itself trap.
 // Recovery has no FP operation and must never acquire a new exception.
 for (unsigned pressure_bits : {0x7f800001u,0x00000001u,0x80000001u})
 for (unsigned normal_bits : {0x412ccccdu,0x00000001u})
 for (unsigned policy : {0u,0x40u,0x8000u,0x8040u}) {
  auto run_child = [&](bool hook) {
   const auto child=fork();assert(child>=0);
   if(child==0) {
    val(cfg,42u);val(cfg+0x1320,normal_bits);val(m+0x800,0x80000000u);
    val(w+0xc00,pressure_bits);
    const unsigned seed=(0x1f80u & ~0x180u) | policy;
    _mm_setcsr(seed);
    (void)capture(m+0x6000,base+(hook?0x1000:0x4000),
                  reinterpret_cast<std::uintptr_t>(w),site==metro::PressureSite::Recovery);
    const auto actual=_mm_getcsr();
    _mm_setcsr(host_mxcsr);
    _exit(actual==seed?0:1);
   }
   int status=0;assert(waitpid(child,&status,0)==child);return status;
  };
  const auto native=run_child(false),hooked=run_child(true);
  assert(native==hooked);
  assert((WIFEXITED(native)&&WEXITSTATUS(native)==0)||
         (site!=metro::PressureSite::Recovery&&WIFSIGNALED(native)&&WTERMSIG(native)==SIGFPE));
 }
 for(unsigned mode=0;mode<18;++mode)for(float pressure:{0.f,-0.f,2.f,10.8f,20.f,-2.f,std::numeric_limits<float>::denorm_min(),std::numeric_limits<float>::infinity(),-std::numeric_limits<float>::infinity(),std::numeric_limits<float>::quiet_NaN()}){
 const bool gate=mode!=0; val(cfg,mode==2?99u:42u); ptr(owner,mode==16?nullptr:mode==3?player:player+0xa0); ptr(m+0xfbd820,mode==17?nullptr:manager); ptr(w+0x118,mode==4?nullptr:owner); ptr(w+0x1b8,mode==5?nullptr:cfg); ptr(manager+0x38,mode==6?nullptr:player); const float normals[]={10.8f,-1.f,0.f,-0.f,std::numeric_limits<float>::quiet_NaN(),std::numeric_limits<float>::infinity(),-std::numeric_limits<float>::infinity(),std::numeric_limits<float>::denorm_min(),2.f}; const float normal=mode>=7&&mode<16?normals[mode-7]:10.8f; const bool correction=(mode==1||(mode>=7&&mode<16))&&std::isfinite(normal)&&normal>0; val(cfg+0x1320,normal); val(m+0x800,gate?0x80000000u:0u);val(w+0xc00,pressure);// Compare actual native bytes with emitted bytes, including every SIMD lane.
 const unsigned mxcsr_seed=0x1f80u | ((state_cases%4)<<13) | ((state_cases%2)?0x8040u:0u) | (state_cases%0x40);
 _mm_setcsr(mxcsr_seed);
 auto native=capture(m+0x6000,base+0x4000,reinterpret_cast<std::uintptr_t>(w),site==metro::PressureSite::Recovery);
 const auto native_mxcsr=_mm_getcsr();
 val(w+0xc00,pressure);
 _mm_setcsr(mxcsr_seed);
 auto hooked=capture(m+0x6000,base+0x1000,reinterpret_cast<std::uintptr_t>(w),site==metro::PressureSite::Recovery);
 assert(_mm_getcsr()==native_mxcsr);
 _mm_setcsr(host_mxcsr);
 assert(native.integer==hooked.integer);
 if(correction && site!=metro::PressureSite::Recovery){unsigned r=site==metro::PressureSite::Consumption1?1:0;float f;std::memcpy(&f,&native.xmm[r][0],4);if(f<normal)std::memcpy(&native.xmm[r][0],cfg+0x1320,4);}
 assert(native.xmm==hooked.xmm);++state_cases;
 val(w+0xc00,pressure);float result;auto target=base+0x1000;auto weapon=reinterpret_cast<std::uintptr_t>(w);float drop=4.f;
 asm volatile("movss %3,%%xmm6; movss %3,%%xmm7; sub $128,%%rsp; mov %2,%%rcx; mov %2,%%rbx; call *%1; add $128,%%rsp; movss %%xmm0,%0":"=m"(result):"r"(target),"r"(weapon),"m"(drop):"rax","rcx","rdx","rbx","r11","xmm0","xmm1","xmm2","xmm6","xmm7","memory","cc");
 if(site==metro::PressureSite::Recovery)std::memcpy(&result,w+0xc00,4);
 else if(site==metro::PressureSite::Consumption1)asm volatile("movss %%xmm1,%0":"=m"(result));
 auto expected=site==metro::PressureSite::Recovery?pressure:pressure-drop;if(correction&&expected<normal)expected=normal;assert(result==expected||(std::isnan(result)&&std::isnan(expected)));++cases;
 }
 }
 munmap(m,0x1000000);std::cout<<cases<<" pressure execution cases and "<<state_cases<<" complete register comparisons passed\n";
}
