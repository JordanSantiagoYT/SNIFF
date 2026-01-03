using Newtonsoft.Json;
using Newtonsoft.Json.Linq;
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;

/*
*  The main class!
*  this is where the magic happens. sorry for the mess!
*  
*  this will keep changing as FNF's chart format
*  evolves over time. Exciting stuff
*  
*  TINY LITTLE TODO of things i wanna do:
*  - convert from MIDI!
*  - crop off the .0s after all the floats. this will
*    probably require making my own JSON deserializer :/
*  - BPM changes to and from automation data
*  - convert from playlist instead of from 1 pattern for
*    ease of use n shit
*  - read song data from inside the flp somewhere
*  - command line arguments
*  
*  pls give credit if u use or reference this or whatever.
*  <3 - MtH
*/

namespace SNIFF
{
	static class Globals
	{
		public const int VersionNumber = 7;
		public const int NoteSize = 24;
		public static ushort ppqn = 96;
        public static float bpmMult = 1;
        public static float susSteps = 4;
        public static string name = "";
		public static float bpm = 0;
		public static List<float> bpmList = new List<float>();
        public static int needsVoices = 0; //-1 = false, 0 = undecided, 1 = true
        public static string player1 = "";
		public static string player2 = "";
		public static string gfVersion = "";
		public static string stage = "";
		public static string arrowSkin = "";
        public static int passPreset = 0; // -1 = preset manual input, 0 = undecided, 1 = preset file
        public static string songCredit = "";
        public static bool trimSus = false; //false = don't trim, true = trim (use for any engine other than h-slice)
        public static int roundDecimal = 6; //amount of decimals to round to!
    }

	public enum MIDINotes
	{
		BF_L = 48,
		BF_D = 49,
		BF_U = 50,
		BF_R = 51,
		
		BF_CAM = 53,
		EN_CAM = 54,
		
		BPM_CH = 56,
		ALT_AN = 57,
		
		EN_L = 60,
		EN_D = 61,
		EN_U = 62,
		EN_R = 63
	}

	public enum FNFNotes : int
	{
		F_L = 0,
		F_D = 1,
		F_U = 2,
		F_R = 3,

		O_L = 4,
		O_D = 5,
		O_U = 6,
		O_R = 7,

		BF_CAM = 8,
		EN_CAM = 9,
		ALT_AN = 10,
		BPM_CH = 11
	}
	

	class Program
	{
		static void ResetGlobals()
		{
			Globals.ppqn = 96;
			Globals.name = "";
			Globals.bpm = 0;
            Globals.bpmMult = 1;
            Globals.susSteps = 4;
            Globals.needsVoices = 0;
			Globals.player1 = "";
			Globals.player2 = "";
			Globals.gfVersion = "";
			Globals.stage = "";
            Globals.songCredit = "";
        }

		public static FLNote MakeNote(double strumTime, int noteData, float sustainLength, bool mustHitSection, float bpm)
		{
			byte velo = 0x64;
			uint noteTime = (uint)Math.Round(strumTime / MIDITimeToMillis(bpm));
			uint duration = (uint)Globals.ppqn / 4;
			uint midiPitch = 0;

			if (sustainLength > 0)
			{
				duration = (uint)(sustainLength / MIDITimeToMillis(bpm));
				if (duration < (uint)Globals.ppqn)
					velo = 0x3F;
			}
			if (noteData >= (int)FNFNotes.BF_CAM)
				duration = (uint)(Globals.ppqn * 4);
			
			switch (noteData)
			{
				case (int)FNFNotes.F_L:
				case (int)FNFNotes.F_D:
				case (int)FNFNotes.F_U:
				case (int)FNFNotes.F_R:
					midiPitch = (uint)(MIDINotes.BF_L + noteData + (mustHitSection ? 0 : 12));
					break;
				case (int)FNFNotes.O_L:
				case (int)FNFNotes.O_D:
				case (int)FNFNotes.O_U:
				case (int)FNFNotes.O_R:
					midiPitch = (uint)(MIDINotes.BF_L + noteData - 4 + (mustHitSection ? 12 : 0));
					break;
				case (int)FNFNotes.BF_CAM:
					midiPitch = (uint)MIDINotes.BF_CAM;
					break;
				case (int)FNFNotes.EN_CAM:
					midiPitch = (uint)MIDINotes.EN_CAM;
					break;
				case (int)FNFNotes.ALT_AN:
					midiPitch = (uint)MIDINotes.ALT_AN;
					break;
				case (int)FNFNotes.BPM_CH:
					midiPitch = (uint)MIDINotes.BPM_CH;
					break;
				default:
					break;
			}

			return new FLNote
			{
				Time = noteTime,
				TBD = 0x4000,
				ChannelNo = 0x0000,
				Duration = duration,
				Pitch = midiPitch,
				FinePitch = 120,
				Release = 0x40,
				Flags = 0x00,
				Panning = 0x40,
				Velocity = velo,
				ModX = 0x80,
				ModY = 0x80
			};
		}

		static FLNote DefaultNote(uint time, uint duration, uint pitch)
		{
			return new FLNote
			{
				Time = time,
				TBD = 0x4000,
				ChannelNo = 0x0000,
				Duration = duration,
				Pitch = pitch,
				FinePitch = 120,
				Release = 0x40,
				Flags = 0x00,
				Panning = 0x40,
				Velocity = 0x64,
				ModX = 0x80,
				ModY = 0x80
			};
		}

		static FLNote DefaultNote()
		{
			return DefaultNote(0, (uint)Globals.ppqn / 4, 60);
		}

        static JObject DefaultSection(bool length)
        {
            JObject section = new JObject { };
            if (length)
            {
                section.Add("lengthInSteps", 16); //sigh
            }
            section.Add("mustHitSection", true);
            section.Add("sectionNotes", JArray.FromObject(new object[][] { }));
            return section;
        }

        static byte[] FLNotesToBytes(List<FLNote> notes)
		{
			List<byte> bytes = new List<byte>();
			foreach (FLNote note in notes)
			{
				bytes.AddRange(BitConverter.GetBytes(note.Time));
				bytes.AddRange(BitConverter.GetBytes(note.TBD));
				bytes.AddRange(BitConverter.GetBytes(note.ChannelNo));
				bytes.AddRange(BitConverter.GetBytes(note.Duration));
				bytes.AddRange(BitConverter.GetBytes(note.Pitch));
				bytes.AddRange(BitConverter.GetBytes(note.FinePitch));
				bytes.Add(note.Release);
				bytes.Add(note.Flags);
				bytes.Add(note.Panning);
				bytes.Add(note.Velocity);
				bytes.Add(note.ModX);
				bytes.Add(note.ModY);
			}
			return bytes.ToArray();
		}

		static List<byte> JSONtoFL(JObject o)
		{
			Console.Write("How is set value of PPQ? (Default is 96, Max is 65535, 0 will set in 96) ");
			try {
				Globals.ppqn = ushort.Parse(Console.ReadLine());
            } catch (FormatException) {
				Globals.ppqn = 96;
			}
			if(Globals.ppqn == 0) Globals.ppqn = 96;

			List<byte> file = new List<byte>()
			{//full FLhd plus FLdt bytes
				0x46, 0x4C, 0x68, 0x64, 0x06, 0x00, 0x00, 0x00, 0x10, 0x00, 0x05, 0x00, 
				(byte)(Globals.ppqn % 256), (byte)(Globals.ppqn / 256), 0x46, 0x4C, 0x64, 0x74
			}; //then append int size of data (below) and then data itself
			List<byte> data = new List<byte>()
			{
				0xC7, 0x07, 0x31, 0x31, 0x2E, 0x31, 0x2E, 0x30, 0x00, 0x1C, 0x03, 0x41, 0x00, 0x00, 0xE0
			}; //then append size of notes and then notes themselves
			List<FLNote> notes = new List<FLNote>();

			Console.WriteLine("\nYour BPM is "+o["song"]["bpm"]);
			Console.WriteLine("\nYour speed is " + o["song"]["speed"]);
			float bpm = (float)o["song"]["bpm"];
			bool mustHitSection = true;
			var lastBPMChangeTime = new {
				u = (uint)0, f = (double)0, s = (int)0
			};

			Stopwatch sw = new Stopwatch();
            sw.Start(); int[] typeCnt = new int[2];
            int length = o["song"]["notes"].Count();

            int i = 0;
            bool isDone = false;

            Task.Run(() => {
                while (!isDone)
                {
                    Console.Write("\x1b[0GInteger Cnt: " + typeCnt[0] + " Double Cnt: " + typeCnt[1] + " Total Notes Cnt: " + notes.Count + " - " + string.Format("{0:P} Done", i / length));
                    Task.Delay(20);
                }
            });

            for (i = 0; i < length; i++)
            {
                // yes the section loop actually.
                // different kind of sex
                JObject section = (JObject)o["song"]["notes"][i];
                if (section["changeBPM"] != null && (bool)section["changeBPM"] && (float)section["bpm"] != bpm)
                {
                    lastBPMChangeTime = new
                    {
                        u = (uint)(i * Globals.ppqn * 4),
                        f = lastBPMChangeTime.f + ((i - lastBPMChangeTime.s) * 4.0f * (1000.0f * 60.0f / bpm)),
                        s = i
                    };
                    bpm = (float)section["bpm"];
                    notes.Add(DefaultNote(lastBPMChangeTime.u, (uint)(Globals.ppqn * 4), (uint)MIDINotes.BPM_CH));
                    Console.WriteLine("BPM change found at bar " + (i + 1) + ", new BPM is " + bpm + ". Keep note of this!");
                }
                if ((bool)section["mustHitSection"] != mustHitSection)
                {
                    mustHitSection = !mustHitSection;
                    notes.Add(DefaultNote((uint)(i * Globals.ppqn * 4), (uint)(Globals.ppqn * 4), (uint)(mustHitSection ? MIDINotes.BF_CAM : MIDINotes.EN_CAM)));
                }
                if (section["altAnim"] != null && (bool)section["altAnim"])
                    notes.Add(DefaultNote((uint)(i * Globals.ppqn * 4), (uint)(Globals.ppqn * 4), (uint)MIDINotes.ALT_AN));

                foreach (JArray fnfNote in section["sectionNotes"])
                {
                    FLNote swagNote = MakeNote((double)fnfNote[0] - lastBPMChangeTime.f, (int)fnfNote[1], (float)fnfNote[2], mustHitSection, bpm);
                    swagNote.Time += lastBPMChangeTime.u;
                    if (fnfNote.Last().Type == JTokenType.Boolean && fnfNote.Last().Value<bool>() == true)
                        swagNote.Flags = 0x10; //set porta for alt anim Note
                    switch (fnfNote.Last().Type)
                    {
                        case JTokenType.Integer:
                            typeCnt[0]++;
                            break;
                        case JTokenType.Float:
                            typeCnt[1]++;
                            break;
                    }
                    notes.Add(swagNote);
                }
            }
            sw.Stop();
            isDone = true;

            Console.WriteLine("\x1b[0GInteger Cnt: " + typeCnt[0] + " Double Cnt: " + typeCnt[1] + " Total Notes Cnt: " + notes.Count);

            byte[] nBytes = FLNotesToBytes(notes);
            // the array length lets goo
            List<byte> arrlen = new List<byte>();
            int len = nBytes.Length;
            while (len > 0)
            {
                arrlen.Add((byte)(len & 0x7f));
                len = len >> 7;
                if (len > 0)
                    arrlen[arrlen.Count - 1] += 0x80;
            }

            data.AddRange(arrlen.ToArray());
            data.AddRange(nBytes);
            file.AddRange(BitConverter.GetBytes(data.Count));
            file.AddRange(data);
            return file;
        }

        static void FlipNoteActor(JObject section)
		{
			for (int i = 0; i < ((JArray)section["sectionNotes"]).Count; i++)
			{
				int s = (int)section["sectionNotes"][i][1];
				if (s > 3)
					s -= 4;
				else
					s += 4;
				section["sectionNotes"][i][1] = s;
			}
		}
		static double MIDITimeToMillis(float bpm)
		{
			return (1000.0 * 60.0 / bpm / Globals.ppqn);
		}

		/* 
		 * This makes a note data event's data into a
		 * list of FLNotes
		 */
		static List<FLNote> BytesToFLNotes(byte[] b)
		{
			List<FLNote> notes = new List<FLNote>();
			int i = 0;
			while (i < b.Length)
			{
				//notes loop
				FLNote n = new FLNote
				{
					Time = BitConverter.ToUInt32(b, i),
					TBD = BitConverter.ToUInt16(b, i + 4),
					ChannelNo = BitConverter.ToUInt16(b, i + 6),
					Duration = BitConverter.ToUInt32(b, i + 8),
					Pitch = BitConverter.ToUInt32(b, i + 12),
					FinePitch = BitConverter.ToUInt16(b, i + 16),
					Release = b[i + 18],
					Flags = b[i + 19],
					Panning = b[i + 20],
					Velocity = b[i + 21],
					ModX = b[i + 22],
					ModY = b[i + 23]
				};
				notes.Add(n);
                
				i += Globals.NoteSize;
			}

            FLNote[] flArray = notes.OrderBy(n => n.Time).ThenBy(n => n.Duration).ToArray(); // sort by time & duration
            notes = flArray.ToList(); // convert back to List<FLNote>
            flArray = null; // free memory

            Console.WriteLine(notes.Count + " notes processed.");
			return notes;
		}

        static string FLtoJSON(List<FLNote> notes, string fileName, bool addLength, string diff, bool doFormat)
        {
            if (notes == null)
                return null;

            // after da data loop
            // let us start assembling the funk
            //Console.WriteLine("\nFirst, we gotta set up some data...");
            JObject song = null;


            if (File.Exists("preset.txt"))
            {
                if (Globals.passPreset == 0)
                { // if we haven't set the preset yet, ask the user
                    Console.WriteLine("You have a preset file for convert to json.");
                    Console.Write("Do you want to use it instead of manual input? (y/N, default y):");
                    Globals.passPreset = Console.ReadLine().ToLower().Trim() == "n" ? -1 : 1;
                }
                if (Globals.passPreset == 1)
                {
                    bool commentMode = false;
                    string[] presetFile = File.ReadAllLines("preset.txt");
                    double speed = 0;
                    int lineCnt = 0;
                    for (int i = 0; i < presetFile.Length; ++i)
                    {
                        if (lineCnt >= 8) break; // only read the first 8 lines without comments
                        string line = presetFile[i].Trim();
                        if (line.StartsWith("//")) continue; // skip comments
                        else if (line.Contains("//")) // remove comments
                        {
                            line = line.Substring(0, line.IndexOf("//")).Trim();
                        }
                        else if (line.StartsWith("/*")) // start comment mode
                        {
                            commentMode = true;
                        }
                        else if (line.Contains("*/") && !line.Contains("\t")) // end comment mode, but ignores if it contains tab
                        {
                            commentMode = false;
                            continue;
                        }

                        if (commentMode) continue;

                        switch (lineCnt)
                        {
                            case 0: // song name
                                if (Globals.name == "") Globals.name = line;
                                if (string.IsNullOrWhiteSpace(Globals.name))
                                {
                                    Globals.name = Path.GetFileNameWithoutExtension(fileName);
                                }
                                break;
                            case 1: // bpm
                                if (Globals.bpm == 0)
                                {
                                    Console.Write("BPM: ");
                                    Globals.bpm = float.Parse(line);
                                }
                                else if (Globals.bpmList.Count > 0)
                                    Globals.bpm = Globals.bpmList[0];
                                break;
                            case 2: // needsVoices
                                if (Globals.needsVoices == 0) Globals.needsVoices = line.ToLower().Trim() == "n" ? -1 : 1;
                                break;
                            case 3: // player1
                                if (Globals.player1 == "") Globals.player1 = line;
                                if (string.IsNullOrWhiteSpace(Globals.player1)) Globals.player1 = "bf";
                                break;
                            case 4: // player2
                                if (Globals.player2 == "") Globals.player2 = line;
                                if (string.IsNullOrWhiteSpace(Globals.player2)) Globals.player2 = "dad";
                                break;
                            case 5: // gfVersion
                                if (Globals.gfVersion == "") Globals.gfVersion = line;
                                if (string.IsNullOrWhiteSpace(Globals.gfVersion)) Globals.gfVersion = "gf";
                                break;
                            case 6: // stage
                                if (Globals.stage == "") Globals.stage = line;
                                if (string.IsNullOrWhiteSpace(Globals.stage)) Globals.stage = "gf";
                                break;
                            case 7: // speed
                                string spd = line;
                                if (!string.IsNullOrWhiteSpace(spd))
                                {
                                    speed = float.Parse(spd);
                                }
                                else
                                {
                                    speed = (Globals.bpm * Globals.bpmMult) / 50;
                                }
                                break;
                            case 8: // song credits
                                if (Globals.songCredit == "") Globals.songCredit = line ?? "";
                                break;
                            default:
                                Console.WriteLine("Unknown preset line: " + line);
                                break;
                        }
                        ++lineCnt;
                    }

                    // settings
                    song = new JObject {
                        { "song", Globals.name }
                    };
                    song.Add("bpm", Globals.bpm);
                    song.Add("needsVoices", Globals.needsVoices > 0);
                    song.Add("player1", Globals.player1);
                    song.Add("player2", Globals.player2);
                    song.Add("gfVersion", Globals.gfVersion);
                    song.Add("stage", Globals.stage);
                    song.Add("speed", speed);
                    song.Add("songCredit", Globals.songCredit);
                }
            }

            // if you don't have a preset file or want to input manually, i will ask you for manual input
            if (!File.Exists("preset.txt") || Globals.passPreset == -1)
            {
                Console.WriteLine(Globals.passPreset == -1 ? "Using manual input." : "No preset file found, using manual input.");

                if (Globals.name == "")
                {
                    Console.Write("Song name (Leave blank to set to the same as the FLP.): ");
                    Globals.name = Console.ReadLine();
                    if (string.IsNullOrWhiteSpace(Globals.name))
                    {
                        Globals.name = Path.GetFileNameWithoutExtension(fileName);
                    }
                }
                song = new JObject {
                        { "song", Globals.name }
                    };
                if (Globals.bpm == 0)
                {
                    Console.Write("BPM: ");
                    Globals.bpm = float.Parse(Console.ReadLine());
                }
                else if (Globals.bpmList.Count > 0)
                    Globals.bpm = Globals.bpmList[0];

                // bpm section
                song.Add("bpm", Globals.bpm * Globals.bpmMult);
                if (Globals.needsVoices == 0)
                {
                    Console.Write("Use separate voices file? (y/N, default y) ");
                    Globals.needsVoices = Console.ReadLine().ToLower().Trim() == "n" ? -1 : 1;
                }
                song.Add("needsVoices", Globals.needsVoices > 0);

                // player1 section
                if (Globals.player1 == "")
                {
                    Console.Write("player1 (playable character like bf): ");
                    Globals.player1 = Console.ReadLine();
                    if (string.IsNullOrWhiteSpace(Globals.player1)) Globals.player1 = "bf";
                }
                song.Add("player1", Globals.player1);

                // player2 section
                if (Globals.player2 == "")
                {
                    Console.Write("player2 (opponent character, see assets\\data\\characterList.txt): ");
                    Globals.player2 = Console.ReadLine();
                    if (string.IsNullOrWhiteSpace(Globals.player2)) Globals.player2 = "dad";
                }
                song.Add("player2", Globals.player2);

                // girlfriend section
                if (Globals.gfVersion == "")
                {
                    Console.Write("gfVersion (gf, gf-car, gf-christmas, gf-pixel): ");
                    Globals.gfVersion = Console.ReadLine();
                    if (string.IsNullOrWhiteSpace(Globals.gfVersion)) Globals.gfVersion = "gf";
                }
                song.Add("gfVersion", Globals.gfVersion);

                // stage section
                if (Globals.stage == "")
                {
                    Console.Write("stage (stage, halloween, philly, limo, mall, mallEvil, school, schoolEvil, tank): ");
                    Globals.stage = Console.ReadLine();
                    if (string.IsNullOrWhiteSpace(Globals.stage)) Globals.stage = "stage";
                }
                song.Add("stage", Globals.stage);

                Console.Write("Scroll speed (if you leave blank, it's auto calculated based on the current bpm): ");
                string spd = Console.ReadLine();
                if (!string.IsNullOrWhiteSpace(spd))
                    song.Add("speed", float.Parse(spd));
                else
                {
                    song.Add("speed", (Globals.bpm * Globals.bpmMult) / 50);
                }

                Console.Write("song credit (only works in js engine): ");
                Globals.songCredit = Console.ReadLine();
                song.Add("songCredit", Globals.songCredit);
            }
            int enableChangeBPM = 0; // 0 = no, 1 = yes, 2 = yes and use bpmList.txt

            for (int i = 0; i < notes.Count; i++)
            {
                if (notes[i].Pitch == (uint)MIDINotes.BPM_CH)
                {
                    Console.Write("\nLooks like you have one or more BPM changes. ");
                    if (File.Exists("bpmList.txt") && Globals.bpmList.Count == 0)
                    {
                        Console.Write("Do you want to use bpmList.txt?\n" +
                            "(y/N, default N) ");
                        if (Console.ReadLine().ToLower().Trim() == "y")
                        {
                            enableChangeBPM = 2;
                            string[] bpmListFile = File.ReadAllLines("bpmList.txt");
                            foreach (string bpmLine in bpmListFile)
                            {
                                bool success = float.TryParse(bpmLine, out float outBPM);
                                if (success)
                                {
                                    Globals.bpmList.Add(outBPM);
                                    Console.WriteLine("Added BPM " + outBPM);
                                }
                            }
                        }
                    }
                    if (Globals.bpmList.Count > 0)
                    {
                        enableChangeBPM = 2;
                        Globals.bpm = Globals.bpmList[0];
                        song["bpm"] = Globals.bpm;
                    }

                    if (enableChangeBPM == 0)
                    {
                        Console.Write("Is the initial BPM of " + Globals.bpm + " correct? If so, leave the\n" +
                            "following field empty. If not, please type the correct BPM.\n" +
                            "BPM: ");
                        string newbpm = Console.ReadLine();
                        if (!string.IsNullOrWhiteSpace(newbpm))
                        {
                            float daBPM = float.Parse(newbpm) * Globals.bpmMult;
                            Globals.bpm = daBPM;
                            song["bpm"] = daBPM;
                            Globals.bpmList.Add(daBPM);
                        }
                        Console.WriteLine("Selected BPM: " + Globals.bpm + "\nGreat! After you select the save directory keep an eye out, we'll be asking you for the new BPMs.");
                        enableChangeBPM = 1;
                    }
                    i = notes.Count;
                }
            }
            Console.WriteLine("");
            string dir = null;

            SaveFileDialog saveBrowser = new SaveFileDialog
            {
                InitialDirectory = dir,
                Filter = "JSON File (*.json)|*.json|All files (*.*)|*.*",
                FileName = Path.GetFileNameWithoutExtension(fileName),
            };
            if (diff != "normal")
                saveBrowser.FileName += "-" + diff;
            saveBrowser.FileName += ".json";

            if (saveBrowser.ShowDialog() != DialogResult.OK)
            {
                Console.WriteLine("Save cancelled. Aborting JSON write.");
                return null;
            }

            string outPath = saveBrowser.FileName;
            dir = Path.GetDirectoryName(outPath);

            // formatted?
            Formatting writerFormatting = doFormat ? Formatting.Indented : Formatting.None;

            using (var fs = new FileStream(outPath, FileMode.Create, FileAccess.Write, FileShare.None))
            using (var sw = new StreamWriter(fs))
            using (var writer = new JsonTextWriter(sw) { Formatting = writerFormatting })
            {
                writer.WriteStartObject(); // root {

                writer.WritePropertyName("song");
                writer.WriteStartObject();

                writer.WritePropertyName("song");
                writer.WriteValue(Globals.name);

                writer.WritePropertyName("bpm");
                double bpmValue = Globals.bpm * Globals.bpmMult;
                if (song != null && song["bpm"] != null)
                    bpmValue = Convert.ToDouble(song["bpm"].ToObject<object>());
                writer.WriteValue(bpmValue);

                writer.WritePropertyName("needsVoices");
                writer.WriteValue(Globals.needsVoices > 0);

                writer.WritePropertyName("player1");
                writer.WriteValue(Globals.player1);

                writer.WritePropertyName("player2");
                writer.WriteValue(Globals.player2);

                writer.WritePropertyName("gfVersion");
                writer.WriteValue(Globals.gfVersion);

                writer.WritePropertyName("stage");
                writer.WriteValue(Globals.stage);

                double speedVal = (song != null && song["speed"] != null) ? Convert.ToDouble(song["speed"].ToObject<object>()) : ((Globals.bpm * Globals.bpmMult) / 50.0);
                writer.WritePropertyName("speed");
                writer.WriteValue(speedVal);

                writer.WritePropertyName("songCredit");
                writer.WriteValue(Globals.songCredit ?? "");

                // begin notes
                writer.WritePropertyName("notes");
                writer.WriteStartArray();

                List<JObject> pendingSections = new List<JObject>();
                bool mustHitSection = true;
                var lastBPMChangeTime = new { u = (uint)0, f = (double)0, s = (int)-1 };
                int bpmListIdx = 1;
                int totalNotes = notes.Count;
                int progress = 0;
                int sectionCnt = 0;

                uint noteData = 0;
                double strumTime = 0;
                double sustainTime = 0;

                JObject lastSection = null;
                JArray curSection = null;

                Stopwatch swatch = new Stopwatch();
                swatch.Start();

                int roundDecimal = Globals.roundDecimal;
                bool isDone = false;

                Task.Run(() =>
                {
                    while (!isDone)
                    {
                        Console.Write($"\x1b[0GNotes: {progress} / {totalNotes} ({progress / (double)totalNotes:P3}) Section: {sectionCnt}");
                        Task.Delay(20).Wait();
                    }
                });

                Action<JObject> FlushSectionToWriter = (JObject sec) =>
                {
                    writer.WriteStartObject();
                    if (sec.ContainsKey("lengthInSteps"))
                    {
                        writer.WritePropertyName("lengthInSteps");
                        writer.WriteValue(sec["lengthInSteps"].ToObject<int>());
                    }

                    // mustHitSection
                    writer.WritePropertyName("mustHitSection");
                    writer.WriteValue(sec["mustHitSection"].ToObject<bool>());

                    // bpm/changeBPM handling
                    if (sec.ContainsKey("bpm") && sec.ContainsKey("changeBPM"))
                    {
                        writer.WritePropertyName("bpm");
                        // ensure numeric
                        writer.WriteValue(Convert.ToDouble(sec["bpm"].ToObject<object>()));
                        writer.WritePropertyName("changeBPM");
                        writer.WriteValue(sec["changeBPM"].ToObject<bool>());
                    }
                    else if (sec.ContainsKey("bpm"))
                    {
                        writer.WritePropertyName("bpm");
                        writer.WriteValue(Convert.ToDouble(sec["bpm"].ToObject<object>()));
                    }
                    else if (sec.ContainsKey("changeBPM"))
                    {
                        writer.WritePropertyName("changeBPM");
                        writer.WriteValue(sec["changeBPM"].ToObject<bool>());
                    }

                    if (sec.ContainsKey("altAnim"))
                    {
                        writer.WritePropertyName("altAnim");
                        writer.WriteValue(sec["altAnim"].ToObject<bool>());
                    }

                    writer.WritePropertyName("sectionNotes");
                    writer.WriteStartArray();

                    if (sec["sectionNotes"] is JArray jarr)
                    {
                        foreach (var noteToken in jarr)
                        {
                            if (noteToken is JArray noteArr)
                            {
                                writer.WriteStartArray();
                                foreach (var v in noteArr)
                                {
                                    switch (v.Type)
                                    {
                                        case JTokenType.Integer:
                                            writer.WriteValue(v.ToObject<long>());
                                            break;
                                        case JTokenType.Float:
                                            writer.WriteValue(v.ToObject<double>());
                                            break;
                                        case JTokenType.Boolean:
                                            writer.WriteValue(v.ToObject<bool>());
                                            break;
                                        case JTokenType.String:
                                            writer.WriteValue(v.ToObject<string>());
                                            break;
                                        default:
                                            writer.WriteValue(v.ToString());
                                            break;
                                    }
                                }
                                writer.WriteEndArray();
                            }
                            else
                            {
                                noteToken.WriteTo(writer);
                            }
                        }
                    }

                    writer.WriteEndArray();
                    writer.WriteEndObject();
                };

                // main processing loop
                for (int iNote = 0; iNote < notes.Count; ++iNote)
                {
                    FLNote daNote = notes[iNote];

                    while (sectionCnt * Globals.ppqn * 4 <= daNote.Time)
                    {
                        if (curSection != null && curSection.Count > 0)
                        {
                            foreach (JObject loop in pendingSections)
                            {
                                FlushSectionToWriter(loop);
                            }
                            pendingSections.Clear();
                        }

                        lastSection = DefaultSection(addLength);
                        pendingSections.Add(lastSection);
                        sectionCnt++;
                        lastSection["mustHitSection"] = mustHitSection;
                        curSection = (JArray)lastSection["sectionNotes"];
                    }

                    // compute strumTime
                    strumTime = Math.Round(
                        lastBPMChangeTime.f + MIDITimeToMillis(Globals.bpm * Globals.bpmMult) * (daNote.Time - lastBPMChangeTime.u),
                        roundDecimal,
                        MidpointRounding.AwayFromZero
                    );

                    // sustain
                    sustainTime = 0;
                    if (daNote.Velocity < 0x40 || daNote.Duration >= Globals.ppqn * (Globals.susSteps / 4))
                    {
                        sustainTime = Math.Round(
                            MIDITimeToMillis(Globals.bpm * Globals.bpmMult) * (daNote.Duration - Globals.ppqn / 4),
                            roundDecimal,
                            MidpointRounding.AwayFromZero
                        );
                        if (sustainTime < 0) sustainTime = 0;
                    }

                    // handle pitch cases
                    switch (daNote.Pitch)
                    {
                        case (uint)MIDINotes.BF_CAM:
                        case (uint)MIDINotes.EN_CAM:
                            lastSection["sectionNotes"] = curSection;
                            mustHitSection = (uint)MIDINotes.BF_CAM == daNote.Pitch;
                            if (lastSection["mustHitSection"].ToObject<bool>() != mustHitSection && curSection.Count > 0)
                            {
                                FlipNoteActor(lastSection);
                            }
                            lastSection["mustHitSection"] = mustHitSection;
                            curSection = (JArray)lastSection["sectionNotes"];
                            break;

                        case (uint)MIDINotes.BPM_CH:
                            if (sectionCnt == lastBPMChangeTime.s)
                            {
                                Console.WriteLine("\nBPM change event found on bar " + sectionCnt + ", but this section\n" +
                                                  "already had a BPM change, so it was ignored.");
                                break;
                            }
                            Console.WriteLine("\nBPM change event found on bar " + sectionCnt + "!");
                            if (enableChangeBPM == 2 && bpmListIdx < Globals.bpmList.Count)
                            {
                                Globals.bpm = Globals.bpmList[bpmListIdx++];
                            }
                            else if (enableChangeBPM == 1)
                            {
                                Console.WriteLine("\nNew BPM (ignore if blank): ");
                                string lineBPM = Console.ReadLine();
                                if (!string.IsNullOrWhiteSpace(lineBPM))
                                {
                                    float daBPM = float.Parse(lineBPM);
                                    Globals.bpm = daBPM;
                                    Globals.bpmList.Add(daBPM);
                                }
                            }

                            if (enableChangeBPM != 0)
                            {
                                if (lastSection.ContainsKey("changeBPM"))
                                {
                                    lastSection["bpm"] = Globals.bpm;
                                }
                                else
                                {
                                    lastSection.Add("bpm", Globals.bpm);
                                    lastSection.Add("changeBPM", true);
                                }
                            }

                            lastBPMChangeTime = new { u = daNote.Time, f = strumTime, s = sectionCnt };
                            break;

                        case (uint)MIDINotes.ALT_AN:
                            lastSection["altAnim"] = true;
                            break;

                        case (uint)MIDINotes.BF_L:
                        case (uint)MIDINotes.BF_D:
                        case (uint)MIDINotes.BF_U:
                        case (uint)MIDINotes.BF_R:
                        case (uint)MIDINotes.EN_L:
                        case (uint)MIDINotes.EN_D:
                        case (uint)MIDINotes.EN_U:
                        case (uint)MIDINotes.EN_R:
                            if (daNote.Pitch >= (uint)MIDINotes.EN_L)
                                noteData = daNote.Pitch - 60u + (mustHitSection ? 4u : 0u);
                            else
                                noteData = daNote.Pitch - 48u + (mustHitSection ? 0u : 4u);

                            var n = new List<object>(4) { strumTime, noteData };
                            if (!Globals.trimSus || (Globals.trimSus && sustainTime > 0))
                                n.Add(sustainTime);

                            if ((notes[0].Flags & 0x10) == 0x10)
                                n.Add(true);

                            if (curSection == null)
                            {
                                lastSection = DefaultSection(addLength);
                                sectionCnt++;
                                lastSection["mustHitSection"] = mustHitSection;
                                curSection = (JArray)lastSection["sectionNotes"];
                            }

                            curSection.Add(JArray.FromObject(n.ToArray()));
                            break;

                        default:
                            break;
                    }

                    ++progress;
                }

                if (curSection != null && curSection.Count > 0 && lastSection != null)
                {
                    FlushSectionToWriter(lastSection);
                    lastSection = null;
                    curSection = null;
                }

                swatch.Stop();
                isDone = true;
                Console.WriteLine($"\x1b[0G{progress} / {totalNotes} Done ({progress / (double)totalNotes:P3}) Current Section: {sectionCnt}");
                Console.WriteLine("Done! Took " + swatch.ElapsedMilliseconds + " ms to process " + totalNotes + " notes.");

                writer.WriteEndArray(); // notes
                writer.WriteEndObject(); // song

                writer.WritePropertyName("generatedBy");
                writer.WriteValue("SNIFF ver." + Globals.VersionNumber);

                writer.WriteEndObject();
                writer.Flush();
            }
            return outPath;
        }

        static void CollectFLPGlobals(FLFile flFile)
		{
			Globals.ppqn = flFile.ppqn;
			DwordEvent tempoEvent = (DwordEvent)flFile.FindFirstEvent(Event.EventIDs.D_PROJ_TMP);
			if (tempoEvent != null)
			{
				Globals.bpm = (uint)tempoEvent.Value / 1000.0f;
				Console.WriteLine("BPM found: " + Globals.bpm);
			}
		}

		static List<FLNote> CollectFLNotes(FLFile flFile, ushort pattern, bool strict = false)
		{
			List<FLNote> notes = new List<FLNote>();

			// if it has a project tempo it's an .flp
			if (flFile.FindFirstEvent(Event.EventIDs.D_PROJ_TMP) != null)
			{
				CollectFLPGlobals(flFile);
				bool triedPat = false;

				// get the first fpc channel and get just the notes from that,
				// if it dont exist just get them from whatever the first channel is
				ushort generator = 0;
				for (int i = 0; i < flFile.eventList.Count; i++)
				{
					if (flFile.eventList[i].ID == (byte)Event.EventIDs.A_PLUG_NAME &&
						((byte[])flFile.eventList[i].Value).SequenceEqual(new byte[] { 0x46, 0x50, 0x43, 0x00 }))
					{
						generator = (ushort)flFile.FindPrevEvent(Event.EventIDs.W_GEN_CH_NO, i).Value;
						i = flFile.eventList.Count;
						Console.WriteLine("FPC channel found at " + generator);
					}
				}

				// scrub pattern for notes from selected channel
				while (notes.Count == 0)
				{
					byte[] noteData = flFile.FindNoteDataByPatternNum(pattern);
					if (noteData != null)
					{
						notes = BytesToFLNotes(noteData);
						for (int i = 0; i < notes.Count; i++)
						{
							// remove any notes not from selected channel
							if (notes[i].ChannelNo != generator)
								notes.RemoveAt(i--);
						}
						if (notes.Count == 0 && !triedPat)
						{
							pattern = 0;
							triedPat = true;
							if (strict)
								return null;
						}
						pattern++;
					}
					else
					{
						Console.WriteLine("No notes found.");
						//Console.ReadLine();
						return null;
					}
				}
				Console.WriteLine("Notes grabbed from pattern " + (pattern - 1));
			}
			else
			{
				// if .fsc file (pattern number is ignored because there's only one pattern with id 0)
				ArrayEvent noteData = (ArrayEvent)flFile.FindFirstEvent(Event.EventIDs.A_NOTE_DATA);
				if (noteData != null)
					notes = BytesToFLNotes((byte[])noteData.Value);
				else
				{
					Console.WriteLine("No notes found.");
					return null;
				}
			}
			return notes;
		}

        //yes the main function
        [STAThread]
        static void Main(string[] args)
        {
            // Enable ANSI Escape Sequences
            var stdout = Console.OpenStandardOutput();
            var con = new StreamWriter(stdout);
            con.AutoFlush = true;
            Console.SetOut(con);
            Console.WriteLine("SiIva Note Importer For FNF (SNIFF)\nquite pungent my dear... version " + Globals.VersionNumber + "\n");

            Console.WriteLine("Do you want to output formatted JSON? (y/N, Default N)");
            bool doFormat = Console.ReadLine().ToLower().Trim() == "y";

            Console.WriteLine("Do you need lengthInSteps Proprety for JSON? (y/N, Default Y)");
            bool addLength = !(Console.ReadLine().ToLower().Trim() == "n");

            Console.Write("How many steps long should a note be before it's classified as a sustain note? (default is 4)");
            string susAmt = Console.ReadLine();
            if (!string.IsNullOrWhiteSpace(susAmt))
                Globals.susSteps = float.Parse(susAmt);
            else
                Globals.susSteps = 4;

            Console.Write("bpm mult (multiplies bpm): ");
            string beatMult = Console.ReadLine();
            if (!string.IsNullOrWhiteSpace(beatMult))
                Globals.bpmMult = float.Parse(beatMult);
            else
                Globals.bpmMult = 1;

            Console.Write("trim sustain note lengths? (y/n, default n. Used to save filesize, but only compatible with H-Slice)");
            Globals.trimSus = (Console.ReadLine().ToLower().Trim() == "y");

            Console.Write("to how many decimal places should strum times be rounded? (number from 0-13, default 6. higher values make strumTime more precise, but increase filesize by a bit): ");
            string decimalPlaces = Console.ReadLine();
            if (!string.IsNullOrWhiteSpace(decimalPlaces))
                Globals.roundDecimal = (int)Math.Min(13, float.Parse(decimalPlaces));
                if (Globals.roundDecimal < 0) Globals.roundDecimal = 6;
            else
                Globals.roundDecimal = 6;

            OpenFileDialog fileBrowser = new OpenFileDialog
            {
                InitialDirectory = Directory.GetCurrentDirectory(),
                Filter = "FL Studio file (*.fsc, *.flp)|*.fsc;*.flp|JSON file (*.json)|*.json|All files (*.*)|*.*",
                Multiselect = true,
                AutoUpgradeEnabled = true,
                DereferenceLinks = true,
                RestoreDirectory = true
            };
            if (args.Length == 0) Console.WriteLine("Select your .fsc, .flp or .json file...");

            if (args.Length > 0 || fileBrowser.ShowDialog() == DialogResult.OK)
            {
                if (args.Length == 0)
                    args = fileBrowser.FileNames;
                string dir = Path.GetDirectoryName(fileBrowser.FileName);
                foreach (string fileName in args)
                {
                    if (fileName.EndsWith(".json"))
                    {
                        Console.WriteLine("Opened JSON file: " + fileName);
                        JObject o;
                        try { o = JObject.Parse(File.ReadAllText(fileName)); }
                        catch (Exception e)
                        {
                            MessageBox.Show(e.Message);
                            return;
                        }

                        byte[] file = JSONtoFL(o).ToArray();

                        dir = Path.GetDirectoryName(fileName);
                        SaveFileDialog saveBrowser = new SaveFileDialog
                        {
                            InitialDirectory = dir,
                            Filter = "FL Studio score file (*.fsc)|*.fsc|All files (*.*)|*.*",
                            FileName = Path.GetFileNameWithoutExtension(fileName) + ".fsc",
                        };
                        if (saveBrowser.ShowDialog() == DialogResult.OK)
                        {
                            File.WriteAllBytes(saveBrowser.FileName, file);
                            dir = Path.GetDirectoryName(saveBrowser.FileName);
                        }
                    }

                    else
                    {
                        byte[] b = null;
                        Console.WriteLine("Reading file...");
                        try { b = File.ReadAllBytes(fileName); }
                        catch (Exception e)
                        {
                            MessageBox.Show(e.Message);
                            return;
                        }
                        if (b == null || b.Length < 4)
                            return;

                        Console.WriteLine("Reading Done");
                        FLFile flFile = new FLFile(b);

                        ushort[] patterns = new ushort[] { 0, 0, 0 };
                        string[] diffnames = new string[] { "easy", "normal", "hard" };
                        bool diffs = false;
                        for (int j = 0; j < diffnames.Length; j++)
                        {
                            patterns[j] = flFile.FindPatternNumByName(diffnames[j]);
                            if (patterns[j] != 0)
                            {
                                diffs = true;
                                Console.WriteLine("Found \"" + diffnames[j] + "\" pattern!");
                            }
                            else
                                Console.WriteLine("No pattern named \"" + diffnames[j] + "\".");
                        }
                        Console.WriteLine();
                        if (!diffs)
                        {
                            WordEvent curPat = (WordEvent)flFile.FindFirstEvent(Event.EventIDs.W_CUR_PAT);
                            if (curPat != null)
                                patterns[0] = (ushort)curPat.Value;
                            else
                                patterns[0] = 1;
                        }

                        for (int i = 0; i < patterns.Length; i++)
                        {
                            if (patterns[i] != 0)
                            {
                                if (diffs)
                                    Console.WriteLine("Current difficulty: " + diffnames[i]);
                                string file = FLtoJSON(CollectFLNotes(flFile, patterns[i], diffs), fileName, addLength, diffnames[i], doFormat);                                
                            }
                        }
                        ResetGlobals();
                    }
                }

                GC.Collect();
                Console.WriteLine("Press any key to close...");
                Console.ReadKey();
                return;
            }
            else Console.WriteLine("Dialog closed");
        }
    }
}
