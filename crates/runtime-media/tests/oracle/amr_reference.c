/* SPDX-License-Identifier: MIT
 * Project-authored deterministic oracle harness. External OpenCORE/VisualOn
 * libraries are development references; production uses Rust codecs only. */
#include <opencore-amrnb/interf_enc.h>
#include <opencore-amrnb/interf_dec.h>
#include <opencore-amrwb/dec_if.h>
#include <vo-amrwbenc/enc_if.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static void le16(FILE *file, int16_t value) { unsigned char bytes[2]={(uint16_t)value&255,((uint16_t)value>>8)&255}; if(fwrite(bytes,1,2,file)!=2)exit(2); }
static FILE *output(const char *dir,const char *name){char path[4096];if(snprintf(path,sizeof(path),"%s/%s",dir,name)>=(int)sizeof(path))exit(2);FILE*f=fopen(path,"wbx");if(!f)exit(2);return f;}
static void generate(const char *dir,int wb){
 FILE*in=output(dir,wb?"wb-input.pcm":"nb-input.pcm");FILE*amr=output(dir,wb?"wb-mode4.amr":"nb-mr122.amr");FILE*pcm=output(dir,wb?"wb-mode4.pcm":"nb-mr122.pcm");
 int samples=wb?320:160;void*enc=wb?E_IF_init():Encoder_Interface_init(0);void*dec=wb?D_IF_init():Decoder_Interface_init();if(!enc||!dec)exit(2);
 fputs(wb?"#!AMR-WB\n":"#!AMR\n",amr);uint32_t random=0x01234567;
 for(int frame=0;frame<50;frame++){short input[320],decoded[320];unsigned char packet[128];
  for(int j=0;j<samples;j++){uint32_t i=(uint32_t)frame*samples+j;random^=random<<13;random^=random>>17;random^=random<<5;int32_t square=(i%80<40)?5000:-5000;int32_t ramp=(int32_t)(i%400)*25-5000;int32_t noise=((int32_t)(random>>24)-128)*16;input[j]=(short)(square+ramp+noise);le16(in,input[j]);}
  int n=wb?E_IF_encode(enc,4,input,packet,0):Encoder_Interface_Encode(enc,MR122,input,packet,1);if(n<=0||n>128)exit(2);if(fwrite(packet,1,(size_t)n,amr)!=(size_t)n)exit(2);
  if(wb)D_IF_decode(dec,packet,decoded,0);else Decoder_Interface_Decode(dec,packet,decoded,0);for(int j=0;j<samples;j++)le16(pcm,decoded[j]);
 }
 if(wb){E_IF_exit(enc);D_IF_exit(dec);}else{Encoder_Interface_exit(enc);Decoder_Interface_exit(dec);}fclose(in);fclose(amr);fclose(pcm);
}
int main(int argc,char**argv){if(argc!=2)return 2;generate(argv[1],0);generate(argv[1],1);return 0;}
