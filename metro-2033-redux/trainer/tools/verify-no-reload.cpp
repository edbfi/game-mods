// Linux x86-64 execution harness for generated Windows register-level hooks.
// No target process is opened; all memory belongs to this test process.
#include "metro/no_reload.hpp"
#include <sys/mman.h>
#include <cassert>
#include <cstring>
#include <iostream>
#include <string>
int main(){
 auto* region=static_cast<unsigned char*>(mmap(nullptr,0x1000000,PROT_READ|PROT_WRITE|PROT_EXEC,MAP_PRIVATE|MAP_ANONYMOUS,-1,0));assert(region!=MAP_FAILED);
 auto base=reinterpret_cast<std::uintptr_t>(region);auto cave=base+0x100;auto control=base+0x1100;
 auto image=metro::make_no_reload_image(base,cave,control,metro::verified_no_reload_classes());assert(image.code.size()<0x1000);
 std::memcpy(region+0x100,image.code.data(),image.code.size());region[metro::no_reload_hook_rva+6]=0xc3;
 alignas(8) unsigned char weapon[0x200]{},config[8]{},owner[8]{},manager[0x40]{},player[0x200]{};
 auto setptr=[](void* p,void* value){std::memcpy(p,&value,8);};auto setid=[](void*p,unsigned value){std::memcpy(p,&value,4);};
 setptr(weapon+0x118,owner);setptr(owner,player+0xa0);setptr(weapon+0x1b8,config);setptr(region+0xfbd820,manager);setptr(manager+0x38,player);
 setid(config,123);setid(region+0xf5d090,123);
 auto run=[&](){unsigned result;auto wp=reinterpret_cast<std::uintptr_t>(weapon);asm volatile("push %%r14; push %%r13; mov %2,%%r14; mov $2,%%r13d; mov $7,%%edi; call *%1; mov %%edi,%0; pop %%r13; pop %%r14":"=r"(result):"r"(cave),"r"(wp):"rax","rcx","rdx","rdi","r13","r14","cc","memory");return result;};
 assert(run()==5);region[0x1100]=1;assert(run()==7);
 setptr(owner,player);assert(run()==5);setptr(owner,player+0xa0);
 setid(config,999);assert(run()==5);setid(config,123);
 setptr(manager+0x38,nullptr);assert(run()==5);setptr(manager+0x38,player);
 setptr(weapon+0x1b8,nullptr);assert(run()==5);
 setptr(weapon+0x1b8,config);
 // Walk every class and every registered alias through the generated selector.
 auto classes=metro::verified_no_reload_classes();
 std::memset(region+0x1100,0,classes.size());
 for(std::size_t i=0;i<classes.size();++i){
  for(auto id:classes[i].class_id_rvas){
   const auto value=static_cast<unsigned>(id);setid(region+id,value);setid(config,value);
   assert(run()==5);region[0x1100+i]=1;assert(run()==7);region[0x1100+i]=0;
  }
 }
 setid(config,123);setid(region+0xf5d090,123);
 // Exercise callback variants with actual indirect setter dispatch.
 alignas(8) unsigned char vt[0x1400]{};
 // Setter: mov eax,edx; ret. Windows and System V agree on explicit register use.
 region[0x1800]=0x89;region[0x1801]=0xd0;region[0x1802]=0xc3;
 setptr(vt+0x1360,region+0x1800);setptr(weapon,vt);
 auto sites=metro::verified_no_reload_sites();
 for(std::size_t i=1;i<sites.size();++i){
  if(sites[i].kind==metro::NoReloadSiteKind::HellsingFire)continue;
  auto cb=metro::make_no_reload_image(base,base+0x2000,control,metro::verified_no_reload_classes(),sites[i]);
  std::memcpy(region+0x2000,cb.code.data(),cb.code.size());
  auto end=sites[i].rva+sites[i].original.size();
  if(i==1)region[end]=0xc3;
  else { // Rejoin the original call [rax+1360], then ret.
   const unsigned char continuation[]={0xff,0x90,0x60,0x13,0,0,0xc3};
   std::memcpy(region+end,continuation,sizeof continuation);
  }
  auto run_cb=[&](){unsigned result;auto wp=reinterpret_cast<std::uintptr_t>(weapon);auto target=base+0x2000;
   asm volatile("push %%rbx; push %%rsi; mov %2,%%rcx; lea 8(%2),%%rsi; mov (%2),%%rbx; mov $7,%%eax; call *%1; mov %%eax,%0; pop %%rsi; pop %%rbx":"=r"(result):"r"(target),"r"(wp):"rax","rcx","rdx","rbx","rsi","cc","memory");return result;};
  region[0x1100]=0;assert(run_cb()==6);region[0x1100]=1;assert(run_cb()==7);
  setptr(owner,player);assert(run_cb()==6);setptr(owner,player+0xa0);
 }
 // Hellsing fire: weapon in RDI, vtable in RBX, loaded count in EAX; rejoins call [rbx+1360].
 bool hellsing_seen=false;
 for(const auto& site:sites){
  if(site.kind!=metro::NoReloadSiteKind::HellsingFire)continue;
  hellsing_seen=true;
  std::size_t helsing=0;while(std::string(classes[helsing].feature)!="no_reload_helsing")++helsing;
  const auto id=classes[helsing].class_id_rvas.front();setid(region+id,4242);setid(config,4242);
  auto hc=metro::make_no_reload_image(base,base+0x3000,control,classes,site);
  std::memcpy(region+0x3000,hc.code.data(),hc.code.size());
  const unsigned char continuation[]={0xff,0x93,0x60,0x13,0,0,0xc3};
  std::memcpy(region+site.rva+site.original.size(),continuation,sizeof continuation);
  auto run_h=[&](std::uintptr_t& receiver){unsigned result;auto wp=reinterpret_cast<std::uintptr_t>(weapon);auto target=base+0x3000;std::uintptr_t rcx_out;
   // Skip the red zone; RCX starts null so the cave must set it from RDI.
   asm volatile("sub $128,%%rsp; mov %3,%%rdi; mov (%3),%%rbx; xor %%ecx,%%ecx; mov $7,%%eax; call *%2; add $128,%%rsp; mov %%eax,%0; mov %%rcx,%1":"=&r"(result),"=&r"(rcx_out):"r"(target),"r"(wp):"rax","rbx","rcx","rdx","rdi","cc","memory");receiver=rcx_out;return result;};
  std::uintptr_t receiver=0;const auto wp=reinterpret_cast<std::uintptr_t>(weapon);
  std::memset(region+0x1100,0,classes.size());
  assert(run_h(receiver)==6&&receiver==wp);
  region[0x1100+helsing]=1;assert(run_h(receiver)==7&&receiver==wp);
  setptr(owner,player);assert(run_h(receiver)==6);setptr(owner,player+0xa0);
  region[0x1100+helsing]=0;region[0x1100]=1;assert(run_h(receiver)==6);region[0x1100]=0;
  setptr(weapon+0x1b8,nullptr);region[0x1100+helsing]=1;assert(run_h(receiver)==6);setptr(weapon+0x1b8,config);region[0x1100+helsing]=0;
 }
 assert(hellsing_seen);
 bool range_rejected=false;try{metro::make_no_reload_image(base,base+0x90000000,control,metro::verified_no_reload_classes());}catch(const std::runtime_error&){range_rejected=true;}assert(range_rejected);
 munmap(region,0x1000000);
 std::cout<<"All "<<sites.size()<<" no-reload caves executed: selected, ordinary and NPC paths passed\n";
}
