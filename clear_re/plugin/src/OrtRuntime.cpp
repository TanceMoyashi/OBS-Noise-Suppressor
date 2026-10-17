#include "OrtRuntime.h"

#ifndef ORT_API_MANUAL_INIT
 #error "ORT_API_MANUAL_INIT must be defined for the whole target (see CMakeLists.txt). \
Otherwise onnxruntime.dll stays an import-table dependency and Windows 11's \
C:\\Windows\\System32\\onnxruntime.dll (ONNX Runtime 1.17 / Windows ML) is loaded \
instead of the bundled runtime, which kills the host process."
#endif

#include <onnxruntime_cxx_api.h>

#include <filesystem>
#include <system_error>
#include <vector>

#ifdef _WIN32
 #include <windows.h>
#endif

namespace dpdfnet
{
    namespace
    {
        // onnxruntime_c_api.h: const OrtApiBase* ORT_API_CALL OrtGetApiBase(void)
        using GetApiBaseFn = const OrtApiBase* (ORT_API_CALL*) ();

        struct OrtState
        {
            bool usable = false;
            std::string description;
            std::string dllPath;
            HMODULE module = nullptr;   // プロセス終了まで意図的に保持する (解放しない)
        };

        // GetModuleHandleEx(FROM_ADDRESS) で「このコードを持つモジュール」=
        // プラグイン DLL 自身 (または検証用 EXE) のパスを得る。JUCE 非依存。
        std::wstring moduleFilePath()
        {
            HMODULE h = nullptr;

            if (! ::GetModuleHandleExW (GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                                            | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                                        reinterpret_cast<LPCWSTR> (&moduleFilePath), &h))
                return {};

            std::vector<wchar_t> buf (32768, L'\0');
            const DWORD n = ::GetModuleFileNameW (h, buf.data(), (DWORD) buf.size());

            return n > 0 ? std::wstring (buf.data(), (size_t) n) : std::wstring();
        }

        std::wstring envValue (const wchar_t* name)
        {
            std::vector<wchar_t> buf (4096, L'\0');
            const DWORD n = ::GetEnvironmentVariableW (name, buf.data(), (DWORD) buf.size());

            return (n > 0 && n < (DWORD) buf.size()) ? std::wstring (buf.data(), (size_t) n)
                                                     : std::wstring();
        }

        std::string toUtf8 (const std::wstring& w)
        {
            if (w.empty())
                return {};

            const int n = ::WideCharToMultiByte (CP_UTF8, 0, w.c_str(), (int) w.size(),
                                                 nullptr, 0, nullptr, nullptr);
            if (n <= 0)
                return {};

            std::string s ((size_t) n, '\0');
            ::WideCharToMultiByte (CP_UTF8, 0, w.c_str(), (int) w.size(), s.data(), n, nullptr, nullptr);
            return s;
        }

        void addCandidates (std::vector<std::wstring>& out, const std::filesystem::path& dir)
        {
            if (dir.empty())
                return;

            // 同梱版の一意な名前を優先。同名 (onnxruntime.dll) は import table を
            // 使わない今は不要だが、手動配置した場合の逃げ道として残す。
            out.push_back ((dir / L"dpdfnet_onnxruntime.dll").wstring());
            out.push_back ((dir / L"onnxruntime.dll").wstring());
        }

        std::vector<std::wstring> candidatePaths()
        {
            std::vector<std::wstring> out;

            // 1) 環境変数 (ファイル指定 or ディレクトリ指定)。切り分け・検証用。
            if (const auto env = envValue (L"DPDFNET_ORT"); ! env.empty())
            {
                std::error_code ec;
                if (std::filesystem::is_directory (env, ec))
                    addCandidates (out, env);
                else
                    out.push_back (env);
            }

            // 2) 自分のバイナリの隣 (VST3 は <bundle>/Contents/x86_64-win/) と 3 階層上
            //    (models/ の探索順とそろえてある)。
            auto dir = std::filesystem::path (moduleFilePath()).parent_path();
            for (int i = 0; i < 4 && ! dir.empty(); ++i)
            {
                addCandidates (out, dir);
                dir = dir.parent_path();
            }

            return out;
        }

        OrtState loadOrt()
        {
            OrtState st;
            std::vector<std::string> notes;

            for (const auto& path : candidatePaths())
            {
                std::error_code ec;
                if (! std::filesystem::exists (path, ec))
                    continue;

                // LOAD_WITH_ALTERED_SEARCH_PATH: この DLL の依存 (VC++ ランタイム等) を
                // 「この DLL のディレクトリ」から探す。素の LoadLibrary だと探索順に
                // プラグインのディレクトリが入らないため、一意名でも保険として付ける。
                HMODULE h = ::LoadLibraryExW (path.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);

                if (h == nullptr)
                {
                    notes.push_back (toUtf8 (path) + ": LoadLibraryExW failed (error "
                                     + std::to_string ((int) ::GetLastError()) + ")");
                    continue;
                }

                const auto getApiBase = reinterpret_cast<GetApiBaseFn> (::GetProcAddress (h, "OrtGetApiBase"));
                const OrtApiBase* base = getApiBase != nullptr ? getApiBase() : nullptr;

                if (base == nullptr || base->GetApi == nullptr)
                {
                    notes.push_back (toUtf8 (path) + ": OrtGetApiBase missing");
                    ::FreeLibrary (h);
                    continue;
                }

                const char* version = base->GetVersionString != nullptr ? base->GetVersionString() : nullptr;
                const OrtApi* api = base->GetApi ((uint32_t) ORT_API_VERSION);

                if (api == nullptr)
                {
                    notes.push_back (toUtf8 (path) + ": runtime "
                                     + (version != nullptr ? version : "?")
                                     + " does not provide API version " + std::to_string (ORT_API_VERSION));
                    ::FreeLibrary (h);
                    continue;
                }

                Ort::InitApi (api);

                st.usable    = true;
                st.module    = h;
                st.dllPath   = toUtf8 (path);
                st.description = "ONNX Runtime " + std::string (version != nullptr ? version : "?")
                               + " (" + st.dllPath + ")";
                return st;
            }

            st.description = "ONNX Runtime could not be loaded";

            if (notes.empty())
                st.description += " (dpdfnet_onnxruntime.dll not found next to "
                                  + ownDirectoryUtf8() + ")";
            else
                for (const auto& note : notes)
                    st.description += "; " + note;

            return st;
        }

        // 関数ローカル static (magic static) なので初期化は 1 回・スレッドセーフ。
        const OrtState& state()
        {
            static const OrtState s = loadOrt();
            return s;
        }
    }

    bool ensureOrtLoaded()
    {
        return state().usable;
    }

    const std::string& ortRuntimeDescription()
    {
        return state().description;
    }

    std::string ownDirectoryUtf8()
    {
        return toUtf8 (std::filesystem::path (moduleFilePath()).parent_path().wstring());
    }
}
