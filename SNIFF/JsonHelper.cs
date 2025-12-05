using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

namespace SNIFF
{
    internal class JsonHelper
    {
        private const string INDENT_STRING = "\t";
        public static string FormatJson(string str)
        {
            int indent = 0; char ch = '\0'; char nextCh = '\0'; int index = 0;
            bool quoted = false; bool escaped = false; bool noFormat = false;
            StringBuilder sb = new StringBuilder();
            for (int i = 0; i < str.Length; i++)
            {
                ch = str[i];
                switch (ch)
                {
                    case '{':
                    case '[':
                        sb.Append(ch);
                        if (!quoted)
                        {
                            nextCh = str[i + 1];
                            ++indent;

                            if (!nextCh.Equals('}') && !nextCh.Equals(']'))
                            {
                                sb.AppendLine();
                                for (int j = 0; j < indent; j++)
                                    sb.Append(INDENT_STRING);
                            }
                            else noFormat = true;
                        }
                        break;
                    case '}':
                    case ']':
                        if (!quoted)
                        {
                            if (!noFormat) sb.AppendLine();
                            --indent;

                            if (!noFormat)
                            {
                                for (int j = 0; j < indent; j++)
                                    sb.Append(INDENT_STRING);
                            }

                            noFormat = false;
                        }
                        sb.Append(ch);
                        break;
                    case '"':
                        sb.Append(ch); escaped = false; index = i;
                        while (index > 0 && str[--index] == '\\')
                            escaped = !escaped;
                        if (!escaped)
                            quoted = !quoted;
                        break;
                    case ',':
                        sb.Append(ch);
                        if (!quoted)
                        {
                            sb.AppendLine();
                            if (!noFormat)
                            {
                                for (int j = 0; j < indent; j++)
                                    sb.Append(INDENT_STRING);
                            }
                        }
                        break;
                    case ':':
                        sb.Append(ch);
                        if (!quoted) sb.Append(" ");
                        break;
                    default:
                        sb.Append(ch);
                        break;
                }
            }
            return sb.ToString();
        }
    }
}